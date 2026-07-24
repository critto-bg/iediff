//! Resource coordinator.
//!
//! [`ResourceManager`] ties the KEY index to the BIF archives: given a resource
//! name and type, it resolves the locator through the KEY, opens (and caches)
//! the owning BIF, and returns the resource's raw bytes. BIFs are opened lazily
//! and kept open, so repeated reads from the same archive reuse one handle.
//!
//! One resource at a time: resolve it, read it, decode it. Finding resources
//! lives in [`search`](crate::search), which reaches the index through
//! [`index_entries`](ResourceManager::index_entries) and
//! [`read_for_scan`](ResourceManager::read_for_scan).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::Reader;
use crate::formats::are::{ARE_TYPE, Are, AreError};
use crate::formats::bcs::{BCS_TYPE, BcsError, Comments, Script};
use crate::formats::bif::{BifArchive, BifError};
use crate::formats::cre::{CRE_TYPE, Cre, CreError, CreNames};
use crate::formats::dlg::{DLG_TYPE, Dlg, DlgError};
use crate::formats::eff::{EFF_TYPE, Eff, EffError};
use crate::formats::ids::{IDS_TYPE, IdsFile};
use crate::formats::itm::{ITM_TYPE, Itm, ItmError};
use crate::formats::key::{KeyError, KeyFile, extension_type, type_extension};
use crate::formats::pro::{PRO_TYPE, Pro, ProError};
use crate::formats::spl::{SPL_TYPE, Spl, SplError};
use crate::formats::sto::{STO_TYPE, Sto, StoError};
use crate::formats::tlk::{TlkError, TlkFile};
use crate::formats::twoda::{TWODA_TYPE, TwoDA, TwoDAError};
use crate::script::{self, Vocabulary};

/// Locale whose `dialog.tlk` strings are resolved against. BG2:EE ships several
/// under `lang/`; English is the default. (Selectable later.)
const DEFAULT_LOCALE: &str = "en_US";

/// Loads a KEY and serves resource bytes out of the BIFs it indexes.
pub struct ResourceManager {
    key: KeyFile,
    /// Root that the KEY's (relative) BIF paths are resolved against.
    game_dir: PathBuf,
    /// BIF source index -> open archive. Populated lazily on first access.
    open_bifs: HashMap<usize, BifArchive>,
    /// The `dialog.tlk` string table, opened lazily on first string lookup.
    dialog: Option<TlkFile>,
    /// Index of the override directory; its resources shadow the BIFs.
    over: Override,
    /// Cache of resolved names, keyed by (UPPERCASE resref, type). A creature
    /// may reference the same spell/item many times.
    name_cache: HashMap<(String, u16), ResourceNames>,
    /// Cache of parsed dictionary 2DAs, keyed by UPPERCASE name. `None` records
    /// a table that could not be loaded, so it is not retried.
    twoda_cache: HashMap<String, Option<TwoDA>>,
    /// Cache of parsed IDS tables, keyed by UPPERCASE name.
    ids_cache: HashMap<String, Option<IdsFile>>,
    /// The script vocabulary, built from ACTION.IDS/TRIGGER.IDS on first use.
    /// Shared rather than copied: every decoded script points at the same one.
    vocabulary: Option<Rc<Vocabulary>>,
}

impl ResourceManager {
    /// Open a KEY file and prepare to serve resources from its game directory
    /// (taken as the KEY file's parent). No BIFs are opened yet.
    pub fn open(key_path: &Path) -> Result<ResourceManager, ResourceError> {
        let data = std::fs::read(key_path)?;
        let key = KeyFile::parse(&data)?;
        let game_dir = key_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));

        let over = Override::scan(&game_dir);

        Ok(ResourceManager {
            key,
            game_dir,
            open_bifs: HashMap::new(),
            dialog: None,
            over,
            name_cache: HashMap::new(),
            twoda_cache: HashMap::new(),
            ids_cache: HashMap::new(),
            vocabulary: None,
        })
    }

    /// The underlying KEY index.
    pub fn key(&self) -> &KeyFile {
        &self.key
    }

    /// A summary of the override directory: how many addressable resources it
    /// holds, how many shadow an archived copy, and a per-type breakdown.
    pub fn override_stats(&self) -> OverrideStats {
        let mut counts: HashMap<u16, usize> = HashMap::new();
        let mut shadowing = 0;
        for entry in &self.over.entries {
            *counts.entry(entry.res_type).or_insert(0) += 1;
            if self.key.lookup(&entry.name, entry.res_type).is_some() {
                shadowing += 1;
            }
        }
        let mut by_type: Vec<(u16, usize)> = counts.into_iter().collect();
        by_type.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        OverrideStats {
            present: self.over.present,
            resources: self.over.entries.len(),
            shadowing,
            by_type,
        }
    }

    /// Load a resource, parsing it when the format is supported and otherwise
    /// returning its metadata. The result records where it was resolved from.
    pub fn load(
        &mut self,
        name: &str,
        res_type: u16,
        source: Source,
    ) -> Result<Loaded, ResourceError> {
        let located =
            self.locate(name, res_type, source)
                .ok_or_else(|| ResourceError::NotFound {
                    name: name.to_string(),
                    res_type,
                })?;

        let origin = located.origin();
        let data = self.read_located(&located)?;
        let resource = self.decode(res_type, &data)?;
        Ok(Loaded { origin, resource })
    }

    /// Load a discrete file straight from disk (the `--file` flag), bypassing
    /// the KEY/override index entirely. Reference resolution still runs against
    /// the established game directory, so a modded resource sitting in a mod
    /// tree resolves its links against the install it will be copied into.
    pub fn load_file(&mut self, path: &Path, res_type: u16) -> Result<Loaded, ResourceError> {
        let data = std::fs::read(path)?;
        let resource = self.decode(res_type, &data)?;
        Ok(Loaded {
            origin: Origin::File(path.to_path_buf()),
            resource,
        })
    }

    /// Resolve a strref to its display string from `dialog.tlk`. The TLK is
    /// opened lazily and kept open for subsequent lookups.
    pub fn string(&mut self, strref: u32) -> Result<String, ResourceError> {
        Ok(self.dialog()?.string(strref)?)
    }

    /// Every `(name, type)` the index knows, as `(override, KEY)`. Kept apart
    /// because the layers shadow each other and only the caller knows whether
    /// `--biff` is in play.
    ///
    /// The names borrow from the index, so a caller needing `&mut self`
    /// afterwards must copy what it wants and drop these first.
    pub(crate) fn index_entries(&self) -> (IndexEntries<'_>, IndexEntries<'_>) {
        let over = self
            .over
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.res_type))
            .collect();
        let key = self
            .key
            .resources()
            .iter()
            .map(|r| (r.name(), r.res_type()))
            .collect();
        (over, key)
    }

    /// Resolve one resource and read its bytes. `None` when it does not resolve
    /// or cannot be read, which a scan skips rather than failing on.
    pub(crate) fn read_for_scan(
        &mut self,
        name: &str,
        res_type: u16,
        source: Source,
    ) -> Option<(Origin, Vec<u8>)> {
        let located = self.locate(name, res_type, source)?;
        let origin = located.origin();
        let bytes = self.read_located(&located).ok()?;
        Some((origin, bytes))
    }

    /// The script call vocabulary for this install. Built once from its own
    /// `ACTION.IDS` / `TRIGGER.IDS`; `None` when either is missing, which
    /// leaves compiled scripts readable but unnamed.
    pub(crate) fn vocabulary(&mut self) -> Option<Rc<Vocabulary>> {
        if self.vocabulary.is_none() {
            // Each table is parsed into an owned value before the next borrow.
            let actions = self.load_ids("ACTION").map(script::signatures);
            let triggers = self.load_ids("TRIGGER").map(script::signatures);
            if let (Some(actions), Some(triggers)) = (actions, triggers) {
                let mut vocabulary = script::from_signatures(actions, triggers);
                // Load only the tables the signatures actually name. A table
                // the install lacks simply leaves those arguments numeric.
                for table in vocabulary.symbol_tables() {
                    if let Some(entries) = self.ids_symbols(&table) {
                        vocabulary.set_symbols(&table, entries);
                    }
                }
                self.vocabulary = Some(Rc::new(vocabulary));
            }
        }
        self.vocabulary.clone()
    }

    /// The `projectl.ids` numbers naming this projectile, for a reverse search:
    /// items and spells fire one by number, never by resref. Often several, as
    /// the table gives one file more than one number.
    pub(crate) fn projectile_numbers(&mut self, resref: &str) -> Vec<i64> {
        self.load_ids("PROJECTL")
            .map(|ids| {
                ids.entries()
                    .iter()
                    .filter(|e| e.name.eq_ignore_ascii_case(resref))
                    .map(|e| e.value)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve a resource to a concrete location: an override file when one
    /// exists (and `source` permits it), otherwise the BIF the KEY points at.
    fn locate(&self, name: &str, res_type: u16, source: Source) -> Option<Located> {
        if source == Source::Auto
            && let Some(path) = self.over.path(name, res_type)
        {
            return Some(Located::Override {
                path: path.to_path_buf(),
            });
        }

        let res = self.key.lookup(name, res_type)?;
        let bif = self.key.bif_name(res.source_index())?.to_string();

        Some(Located::Bif {
            source: res.source_index(),
            file_index: res.file_index(),
            res_type: res.res_type(),
            bif,
        })
    }

    /// Parse a resource from its raw bytes and resolve its referenced names,
    /// independent of where the bytes came from (BIF, override, or a discrete
    /// file). Unsupported types fall back to a type/size summary.
    fn decode(&mut self, res_type: u16, data: &[u8]) -> Result<Resource, ResourceError> {
        let resource = match res_type {
            IDS_TYPE => Resource::Ids(IdsFile::parse(data)),
            TWODA_TYPE => Resource::TwoDA(TwoDA::parse(data)?),
            BCS_TYPE => {
                let mut script = Script::parse_verified(data)?;
                if let Some(vocabulary) = self.vocabulary() {
                    // Resolve the strref texts and resource names the view shows
                    // after each call, which the writer cannot reach itself.
                    let comments = self.script_comments(&script, &vocabulary);
                    script.set_vocabulary(vocabulary);
                    script.set_comments(comments);
                }
                Resource::Bcs(script)
            }
            EFF_TYPE => {
                let mut eff = Eff::parse(data)?;
                // The primary/secondary types index dictionary tables; resolve
                // them to symbolic names for the human-readable view.
                let primary = self.dictionary_label("MSCHOOL", eff.primary_type());
                let secondary = self.dictionary_label("MSECTYPE", eff.secondary_type());
                eff.set_type_names(primary, secondary);
                Resource::Eff(eff)
            }
            ITM_TYPE => {
                let mut itm = Itm::parse(data)?;
                // Each ability's primary/secondary type indexes the same
                // dictionary tables as an EFF; resolve them per ability.
                let codes: Vec<(u32, u32)> = itm
                    .ext_headers()
                    .iter()
                    .map(|ext| (ext.primary_type(), ext.secondary_type()))
                    .collect();
                let names: Vec<(Option<String>, Option<String>)> = codes
                    .into_iter()
                    .map(|(primary, secondary)| {
                        (
                            self.dictionary_label("MSCHOOL", primary),
                            self.dictionary_label("MSECTYPE", secondary),
                        )
                    })
                    .collect();
                for (ext, (primary, secondary)) in itm.ext_headers_mut().iter_mut().zip(names) {
                    ext.set_type_names(primary, secondary);
                }

                // Resolve the item's display names from the TLK.
                let (unid, id) = itm.name_strrefs();
                let unid = self.string_label(unid);
                let id = self.string_label(id);
                itm.set_names(unid, id);

                Resource::Itm(itm)
            }
            SPL_TYPE => {
                let mut spl = Spl::parse(data)?;

                let primary = self.dictionary_label("MSCHOOL", spl.primary_type());
                let secondary = self.dictionary_label("MSECTYPE", spl.secondary_type());
                spl.set_type_names(primary, secondary);

                let (unid, id) = spl.name_strrefs();
                let unid = self.string_label(unid);
                let id = self.string_label(id);
                spl.set_names(unid, id);

                let (unid, id) = spl.description_strrefs();
                let unid = self.string_label(unid);
                let id = self.string_label(id);
                spl.set_descriptions(unid, id);

                let projectiles: Vec<Option<i64>> = spl
                    .ext_headers()
                    .iter()
                    .map(|ext| ext.projectile_ids())
                    .collect();
                let names: Vec<Option<String>> = projectiles
                    .into_iter()
                    .map(|projectile| self.ids_label("PROJECTL", projectile?))
                    .collect();
                for (ext, name) in spl.ext_headers_mut().iter_mut().zip(names) {
                    ext.set_projectile_name(name);
                }

                Resource::Spl(spl)
            }
            CRE_TYPE => {
                let mut cre = Cre::parse(data)?;

                // Identity: names from the TLK, the IDS octet and animation from
                // their IDS tables.
                let (long, short) = cre.name_strrefs();
                let codes = cre.ids_codes();
                let names = CreNames {
                    long_name: self.string_label(long),
                    short_name: self.string_label(short),
                    ea: self.ids_label("EA", codes.ea as i64),
                    general: self.ids_label("GENERAL", codes.general as i64),
                    race: self.ids_label("RACE", codes.race as i64),
                    class: self.ids_label("CLASS", codes.class as i64),
                    specific: self.ids_label("SPECIFIC", codes.specific as i64),
                    gender: self.ids_label("GENDER", codes.gender as i64),
                    alignment: self.ids_label("ALIGNMEN", codes.alignment as i64),
                    animation: self.ids_label("ANIMATE", codes.animation as i64),
                    // KIT.IDS is indexed by the kit dword with its high
                    // and low 16-bit halves swapped (so e.g. the
                    // stored 0x40000000 → 0x00004000 = TRUECLASS, and the
                    // low-word 0x00004000 → 0x40000000 = BARBARIAN).
                    kit: self.ids_label("KIT", codes.kit.rotate_left(16) as i64),
                };
                cre.set_names(names);

                // Resolve display names for referenced spells (SPL) and items (ITM).
                self.resolve_refs(
                    cre.known_spells_mut(),
                    SPL_TYPE,
                    |s| s.resource(),
                    |s, n| s.set_name(n),
                );
                self.resolve_refs(
                    cre.memorized_spells_mut(),
                    SPL_TYPE,
                    |s| s.resource(),
                    |s, n| s.set_name(n),
                );
                self.resolve_refs(
                    cre.items_mut(),
                    ITM_TYPE,
                    |it| it.resource(),
                    |it, n| it.set_name(n),
                );

                Resource::Cre(cre)
            }
            PRO_TYPE => {
                let mut pro = Pro::parse(data)?;

                // Header-level lookups: particle color (SPRKLCLR.2DA), the
                // targeting message (TLK), and the miss/success spell names.
                let particle = self.dictionary_label("SPRKLCLR", pro.particle_color() as u32);
                let message = self.string_label(pro.message_strref());
                let default = pro.default_spell().to_string();
                let success = pro.success_spell().to_string();
                let default_name = self
                    .resource_names(&default, SPL_TYPE)
                    .display()
                    .map(str::to_string);
                let success_name = self
                    .resource_names(&success, SPL_TYPE)
                    .display()
                    .map(str::to_string);
                pro.set_header_names(particle, message, default_name, success_name);

                // Travel struct: the smoke animation indexes animate.ids.
                if let Some(code) = pro.travel_smoke() {
                    let name = self.ids_label("ANIMATE", code as i64);
                    pro.set_travel_smoke_name(name);
                }

                // Area struct: fragment/smoke via animate.ids, the two
                // projectiles via projectl.ids (the secondary off by one, like
                // an SPL ext-header), the explosion effect via fireball.ids.
                if let Some(codes) = pro.area_codes() {
                    let fragment = self.ids_label("ANIMATE", codes.fragment_animation as i64);
                    let secondary =
                        self.ids_label("PROJECTL", codes.secondary_projectile as i64 - 1);
                    let explosion = codes
                        .explosion_projectile_ids()
                        .and_then(|n| self.ids_label("PROJECTL", n));
                    let effect = self.ids_label("FIREBALL", codes.explosion_effect as i64);
                    pro.set_area_names(fragment, secondary, explosion, effect);
                }

                Resource::Pro(pro)
            }
            STO_TYPE => {
                let mut sto = Sto::parse(data)?;

                // The store's own name from the TLK.
                let name = self.string_label(sto.name_strref());
                sto.set_name(name);

                // Sale items reference ITMs and cures reference SPLs; resolve
                // their display names the same way creature inventories do.
                self.resolve_refs(
                    sto.sale_mut(),
                    ITM_TYPE,
                    |s| s.resource(),
                    |s, n| s.set_name(n),
                );
                self.resolve_refs(
                    sto.cures_mut(),
                    SPL_TYPE,
                    |c| c.resource(),
                    |c, n| c.set_name(n),
                );

                // Each drink names itself through the TLK.
                let strrefs: Vec<u32> = sto.drinks_mut().iter().map(|d| d.name_strref()).collect();
                let names: Vec<Option<String>> =
                    strrefs.into_iter().map(|s| self.string_label(s)).collect();
                for (drink, name) in sto.drinks_mut().iter_mut().zip(names) {
                    drink.set_name(name);
                }

                Resource::Sto(sto)
            }
            ARE_TYPE => {
                let mut are = Are::parse(data)?;

                // Actors reference CRE files; the flat item pool references
                // ITMs — resolve their display names like inventories do.
                self.resolve_refs(
                    are.actors_mut(),
                    CRE_TYPE,
                    |a| a.resource(),
                    |a, n| a.set_name(n),
                );
                self.resolve_refs(
                    are.items_mut(),
                    ITM_TYPE,
                    |it| it.resource(),
                    |it, n| it.set_name(n),
                );

                // Actor animations index animate.ids.
                let anims: Vec<u32> = are.actors_mut().iter().map(|a| a.animation()).collect();
                let names: Vec<Option<String>> = anims
                    .into_iter()
                    .map(|code| self.ids_label("ANIMATE", code as i64))
                    .collect();
                for (actor, name) in are.actors_mut().iter_mut().zip(names) {
                    actor.set_animation_name(name);
                }

                // Each spawn point names up to ten creatures (or spawngrp.2da
                // entries, which simply won't resolve).
                for spawn in are.spawns_mut() {
                    self.resolve_refs(
                        spawn.creatures_mut(),
                        CRE_TYPE,
                        |c| c.resource(),
                        |c, n| c.set_name(n),
                    );
                }

                // The rest-interruption table names its own creatures.
                if let Some(rest) = are.rest_mut() {
                    self.resolve_refs(
                        rest.creatures_mut(),
                        CRE_TYPE,
                        |c| c.resource(),
                        |c, n| c.set_name(n),
                    );
                }

                // A projectile trap's missile indexes projectl.ids; the stored
                // value is the ids number less one.
                let missiles: Vec<u16> = are.proj_traps_mut().iter().map(|t| t.missile()).collect();
                let names: Vec<Option<String>> = missiles
                    .into_iter()
                    .map(|m| self.ids_label("PROJECTL", m as i64 + 1))
                    .collect();
                for (trap, name) in are.proj_traps_mut().iter_mut().zip(names) {
                    trap.set_missile_name(name);
                }

                // Strref-labelled sections: region info text, door speakers,
                // automap notes.
                self.resolve_strrefs(are.regions_mut(), |r| r.strref(), |r, n| r.set_label(n));
                self.resolve_strrefs(are.doors_mut(), |d| d.strref(), |d, n| d.set_label(n));
                self.resolve_strrefs(are.automap_mut(), |n| n.strref(), |n, l| n.set_label(l));

                Resource::Are(are)
            }
            DLG_TYPE => {
                let mut dlg = Dlg::parse(data)?;

                // Resolve the actor lines and the player/journal text of each
                // transition against the TLK for the human view.
                self.resolve_strrefs(dlg.states_mut(), |s| s.strref(), |s, l| s.set_label(l));
                self.resolve_strrefs(
                    dlg.transitions_mut(),
                    |t| t.player_strref().unwrap_or(u32::MAX),
                    |t, l| t.set_player_label(l),
                );
                self.resolve_strrefs(
                    dlg.transitions_mut(),
                    |t| t.journal_strref().unwrap_or(u32::MAX),
                    |t, l| t.set_journal_label(l),
                );

                Resource::Dlg(dlg)
            }
            _ => Resource::Other(ResourceInfo {
                res_type,
                size: data.len() as u32,
            }),
        };

        Ok(resource)
    }

    /// Read a located resource's raw bytes — from disk (override) or the archive.
    fn read_located(&mut self, located: &Located) -> Result<Vec<u8>, ResourceError> {
        match located {
            Located::Override { path, .. } => Ok(std::fs::read(path)?),
            Located::Bif {
                source,
                file_index,
                res_type,
                ..
            } => {
                let archive = self.archive_for(*source)?;
                Ok(archive.read_resource(*file_index, *res_type)?)
            }
        }
    }

    /// Best-effort resolve a strref to its TLK string for display. `None` for
    /// the -1 "no string" sentinel, on any lookup error, or when the string is
    /// empty.
    fn string_label(&mut self, strref: u32) -> Option<String> {
        if strref == u32::MAX {
            return None;
        }
        let string = self.string(strref).ok()?;
        let trimmed = string.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Resolve a numeric value to its row label in a dictionary 2DA (e.g.
    /// MSCHOOL, MSECTYPE), where the value is the 0-based row index.
    fn dictionary_label(&mut self, table: &str, value: u32) -> Option<String> {
        self.load_2da(table)?
            .row_label(value as usize)
            .map(str::to_string)
    }

    /// Borrow a parsed 2DA by name, honouring the override layer. Parsed once
    /// and cached; a load failure is cached as `None` so it is not retried.
    fn load_2da(&mut self, name: &str) -> Option<&TwoDA> {
        let key = name.to_ascii_uppercase();
        if !self.twoda_cache.contains_key(&key) {
            let parsed = self.parse_2da(name);
            self.twoda_cache.insert(key.clone(), parsed);
        }
        self.twoda_cache.get(&key)?.as_ref()
    }

    fn parse_2da(&mut self, name: &str) -> Option<TwoDA> {
        let located = self.locate(name, TWODA_TYPE, Source::Auto)?;
        let bytes = self.read_located(&located).ok()?;
        TwoDA::parse(&bytes).ok()
    }

    /// Resolve a numeric value to its symbol in an IDS table (e.g. PROJECTL),
    /// where the value indexes by its IDS number. `None` when unresolved.
    fn ids_label(&mut self, table: &str, value: i64) -> Option<String> {
        self.load_ids(table)?
            .lookup_by_value(value)
            .map(str::to_string)
    }

    /// Resolve the end-of-line comments a decompiled script shows: a strref's
    /// text (and sound) and a named resource's display name. References are
    /// collected first, then resolved, so the script borrow is released before
    /// the manager is used.
    fn script_comments(&mut self, script: &Script, vocabulary: &Vocabulary) -> Comments {
        let mut strrefs = Vec::new();
        let mut resources = Vec::new();
        let mut objects = Vec::new();
        script.for_each_reference(
            vocabulary,
            |value| strrefs.push(value),
            |kind, resref| resources.push((kind, resref.to_string())),
            |name| objects.push(name.to_string()),
        );

        let mut comments = Comments::default();
        for value in strrefs {
            comments
                .strrefs
                .entry(value)
                .or_insert_with(|| self.strref_comment(value));
        }
        for (kind, resref) in resources {
            let key = (kind, resref.to_ascii_uppercase());
            if !comments.resources.contains_key(&key)
                && let Some(name) = self.resource_name_comment(kind, &resref)
            {
                comments.resources.insert(key, name);
            }
        }
        // A death variable doubles as the creature's resref, so its name is the
        // creature's display name — nothing shown when it names no creature.
        for death_var in objects {
            let key = death_var.to_ascii_uppercase();
            if !comments.objects.contains_key(&key)
                && let Some(name) = self.resource_names(&death_var, CRE_TYPE).display()
            {
                let name = format!("~{name}~");
                comments.objects.insert(key, name);
            }
        }
        comments
    }

    /// The comment a strref earns: its `~text~`, with the sound that plays with
    /// it in brackets. The "no string" sentinel and an out-of-range index both
    /// read as WeiDU's `<Invalid Strref -1>`.
    fn strref_comment(&mut self, strref: u32) -> String {
        const INVALID: &str = "<Invalid Strref -1>";
        if strref == u32::MAX {
            return INVALID.to_string();
        }
        let Ok(text) = self.string(strref) else {
            return INVALID.to_string();
        };
        let sound = self
            .dialog()
            .ok()
            .and_then(|tlk| tlk.sound(strref).map(str::to_string));
        match sound {
            Some(sound) => format!("~{text}~ [{sound}]"),
            None => format!("~{text}~"),
        }
    }

    /// The comment a named resource earns: its `~display name~`. Only items,
    /// spells and creatures carry a name. A resource that exists but whose name
    /// strref is invalid reads as `<Invalid Strref -1>`, as in WeiDU; a resref
    /// that resolves to no file gets no comment at all.
    fn resource_name_comment(&mut self, kind: u16, resref: &str) -> Option<String> {
        if !matches!(kind, ITM_TYPE | SPL_TYPE | CRE_TYPE) {
            return None;
        }
        match self.resource_names(resref, kind).display() {
            Some(name) => Some(format!("~{name}~")),
            None => self
                .locate(resref, kind, Source::Auto)
                .map(|_| "<Invalid Strref -1>".to_string()),
        }
    }

    /// A table's value -> symbol mapping, for naming script arguments.
    ///
    /// The *last* alias wins where a value repeats, which is the opposite of
    /// [`IdsFile::lookup_by_value`]. `CLASS.IDS` lists `202 LONG_BOW` before
    /// `202 MAGE_ALL`, and a script asking about class 202 means the kit, not
    /// the weapon — the reading the engine's own decompiler gives.
    fn ids_symbols(&mut self, table: &str) -> Option<HashMap<i64, String>> {
        let ids = self.load_ids(table)?;
        let symbols = ids
            .entries()
            .iter()
            .map(|entry| (entry.value, entry.name.clone()))
            .collect();
        Some(symbols)
    }

    /// Borrow a parsed IDS table by name. Parsed once and cached.
    fn load_ids(&mut self, name: &str) -> Option<&IdsFile> {
        let key = name.to_ascii_uppercase();
        if !self.ids_cache.contains_key(&key) {
            let parsed = self.parse_ids(name);
            self.ids_cache.insert(key.clone(), parsed);
        }
        self.ids_cache.get(&key)?.as_ref()
    }

    fn parse_ids(&mut self, name: &str) -> Option<IdsFile> {
        let located = self.locate(name, IDS_TYPE, Source::Auto)?;
        let bytes = self.read_located(&located).ok()?;
        Some(IdsFile::parse(&bytes))
    }

    /// Resolve display names for a slice of records that reference another
    /// resource by resref (e.g. a creature's spells or items). `get` reads the
    /// resref, `set` stores the resolved name back.
    fn resolve_refs<T>(
        &mut self,
        items: &mut [T],
        res_type: u16,
        get: impl Fn(&T) -> &str,
        set: impl Fn(&mut T, Option<String>),
    ) {
        let refs: Vec<String> = items.iter().map(|t| get(t).to_string()).collect();
        let names: Vec<Option<String>> = refs
            .iter()
            .map(|r| {
                self.resource_names(r, res_type)
                    .display()
                    .map(str::to_string)
            })
            .collect();
        for (t, name) in items.iter_mut().zip(names) {
            set(t, name);
        }
    }

    /// Resolve TLK strings for a slice of records that each carry a strref (e.g.
    /// a region's info text or an automap note). `get` reads the strref, `set`
    /// stores the resolved label back.
    fn resolve_strrefs<T>(
        &mut self,
        items: &mut [T],
        get: impl Fn(&T) -> u32,
        set: impl Fn(&mut T, Option<String>),
    ) {
        let refs: Vec<u32> = items.iter().map(&get).collect();
        let names: Vec<Option<String>> = refs.into_iter().map(|s| self.string_label(s)).collect();
        for (t, name) in items.iter_mut().zip(names) {
            set(t, name);
        }
    }

    /// Resolved identified/unidentified names for a referenced resource (ITM/SPL),
    /// read from its header strrefs without a full parse. Cached, since references
    /// repeat (a creature may memorize the same spell hundreds of times).
    fn resource_names(&mut self, resref: &str, res_type: u16) -> ResourceNames {
        if resref.is_empty() {
            return ResourceNames::default();
        }

        let key = (resref.to_ascii_uppercase(), res_type);
        if let Some(cached) = self.name_cache.get(&key) {
            return cached.clone();
        }

        let names = self.read_resource_names(resref, res_type);
        self.name_cache.insert(key, names.clone());
        names
    }

    /// Resolve an ITM/SPL's identified (0x0c) and unidentified (0x08) name
    /// strrefs against the TLK, without a full parse.
    fn read_resource_names(&mut self, resref: &str, res_type: u16) -> ResourceNames {
        let Some(located) = self.locate(resref, res_type, Source::Auto) else {
            return ResourceNames::default();
        };
        let Ok(bytes) = self.read_located(&located) else {
            return ResourceNames::default();
        };
        let reader = Reader::new(&bytes);
        let identified = reader.u32_at(0x0c, "identified name").ok();
        let unidentified = reader.u32_at(0x08, "unidentified name").ok();
        ResourceNames {
            identified: identified.and_then(|s| self.string_label(s)),
            unidentified: unidentified.and_then(|s| self.string_label(s)),
        }
    }

    /// Borrow the dialog TLK, opening it on first use from the locale's
    /// `lang/<locale>/dialog.tlk`.
    fn dialog(&mut self) -> Result<&mut TlkFile, ResourceError> {
        match self.dialog {
            Some(ref mut tlk) => Ok(tlk),
            None => {
                let path = self
                    .game_dir
                    .join("lang")
                    .join(DEFAULT_LOCALE)
                    .join("dialog.tlk");
                Ok(self.dialog.insert(TlkFile::open(&path)?))
            }
        }
    }

    /// Borrow the archive for a BIF source index, opening and caching it on
    /// first use.
    fn archive_for(&mut self, source: usize) -> Result<&mut BifArchive, ResourceError> {
        if !self.open_bifs.contains_key(&source) {
            let bif_name = self
                .key
                .bif_name(source)
                .ok_or(ResourceError::BifIndexOutOfRange(source))?;

            // KEY paths are relative to the game root, possibly with `\` separators.
            let path = self.game_dir.join(bif_name.replace('\\', "/"));
            let archive = BifArchive::open(&path)?;
            self.open_bifs.insert(source, archive);
        }

        self.open_bifs
            .get_mut(&source)
            .ok_or(ResourceError::BifIndexOutOfRange(source))
    }
}

/// The display names a resource (ITM/SPL) carries, resolved from its header
/// strrefs. Both are kept so a consumer can pick whichever it needs; most want
/// [`display`](ResourceNames::display).
#[derive(Clone, Default)]
pub struct ResourceNames {
    pub identified: Option<String>,
    pub unidentified: Option<String>,
}

impl ResourceNames {
    /// The name to show by default: identified if resolved, else unidentified.
    pub fn display(&self) -> Option<&str> {
        self.identified.as_deref().or(self.unidentified.as_deref())
    }
}

/// One index layer's `(name, type)` entries. See
/// [`ResourceManager::index_entries`].
pub(crate) type IndexEntries<'a> = Vec<(&'a str, u16)>;

/// Where to resolve a resource from. `Auto` prefers an override file over the
/// archived original; `Biff` ignores the override entirely (the `--biff` flag).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Auto,
    Biff,
}

/// Where a resolved resource actually came from.
#[derive(Clone)]
pub enum Origin {
    /// A file in the override directory (its path).
    Override(PathBuf),
    /// A BIF archive, by the name the KEY lists.
    Bif(String),
    /// A discrete file read straight from disk (`--file`), bypassing the index.
    File(PathBuf),
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Override(path) => write!(f, "override: {}", path.display()),
            Origin::Bif(bif) => write!(f, "biff: {bif}"),
            Origin::File(path) => write!(f, "file: {}", path.display()),
        }
    }
}

/// A loaded resource together with where it was resolved from.
pub struct Loaded {
    pub origin: Origin,
    pub resource: Resource,
}

/// A loaded resource: a parsed view for formats we understand, otherwise the
/// basic metadata. Renders itself via [`Display`](fmt::Display), so callers
/// print it without knowing the concrete format.
// Variants differ a lot in size (CRE is the largest), but exactly one short-lived
// value exists per CLI invocation, so the layout doesn't matter — boxing would
// only add indirection for no benefit.
#[allow(clippy::large_enum_variant)]
pub enum Resource {
    Ids(IdsFile),
    TwoDA(TwoDA),
    Eff(Eff),
    Itm(Itm),
    Spl(Spl),
    Cre(Cre),
    Pro(Pro),
    Sto(Sto),
    Are(Are),
    Dlg(Dlg),
    Bcs(Script),
    Other(ResourceInfo),
}

impl Resource {
    /// Write the machine-readable view: flat `key=value` lines for the binary
    /// formats. The text formats (IDS, 2DA) are already field-oriented, so they
    /// keep their tabular rendering, minus the human view's reading aids.
    pub fn write_raw(&self, out: &mut dyn Write) -> io::Result<()> {
        match self {
            Resource::Eff(eff) => eff.write_raw(out, ""),
            Resource::Itm(itm) => itm.write_raw(out),
            Resource::Spl(spl) => spl.write_raw(out),
            Resource::Cre(cre) => cre.write_raw(out),
            Resource::Pro(pro) => pro.write_raw(out),
            Resource::Sto(sto) => sto.write_raw(out),
            Resource::Are(are) => are.write_raw(out),
            Resource::Dlg(dlg) => dlg.write_raw(out),
            Resource::Bcs(script) => script.write_raw(out),
            Resource::Ids(ids) => write!(out, "{ids}"),
            Resource::TwoDA(table) => table.write_raw(out),
            Resource::Other(info) => write!(out, "{info}"),
        }
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Resource::Ids(ids) => write!(f, "{ids}"),
            Resource::TwoDA(table) => write!(f, "{table}"),
            Resource::Eff(eff) => write!(f, "{eff}"),
            Resource::Itm(itm) => write!(f, "{itm}"),
            Resource::Spl(spl) => write!(f, "{spl}"),
            Resource::Cre(cre) => write!(f, "{cre}"),
            Resource::Pro(pro) => write!(f, "{pro}"),
            Resource::Sto(sto) => write!(f, "{sto}"),
            Resource::Are(are) => write!(f, "{are}"),
            Resource::Dlg(dlg) => write!(f, "{dlg}"),
            Resource::Bcs(script) => write!(f, "{script}"),
            Resource::Other(info) => write!(f, "{info}"),
        }
    }
}

/// Where a resource lives and how big it is, without its data.
pub struct ResourceInfo {
    pub res_type: u16,
    pub size: u32,
}

impl fmt::Display for ResourceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "  type: 0x{:04x} ({})",
            self.res_type,
            type_extension(self.res_type)
        )?;
        writeln!(f, "  size: {} bytes", self.size)
    }
}

/// Summary of the override directory. See [`ResourceManager::override_stats`].
pub struct OverrideStats {
    /// Whether the `override` directory exists at all.
    pub present: bool,
    /// Addressable resources found (files whose extension names a known type).
    pub resources: usize,
    /// How many of those shadow a resource the KEY also lists.
    pub shadowing: usize,
    /// (resource type, count), most common first.
    pub by_type: Vec<(u16, usize)>,
}

impl fmt::Display for OverrideStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.present {
            return writeln!(f, "override: not present");
        }

        if self.resources == 0 {
            return writeln!(f, "override: empty");
        }

        let fresh = self.resources - self.shadowing;
        writeln!(
            f,
            "override: {} resources ({} new, {} shadowing archived)",
            self.resources, fresh, self.shadowing
        )?;

        writeln!(f, "override by type:")?;
        for (res_type, count) in &self.by_type {
            writeln!(
                f,
                "  {:<5} (0x{:04x})  {:>6}",
                type_extension(*res_type),
                res_type,
                count
            )?;
        }

        Ok(())
    }
}

/// A resource resolved to a concrete place, ready to read.
enum Located {
    Override {
        path: PathBuf,
    },
    Bif {
        source: usize,
        file_index: u32,
        res_type: u16,
        bif: String,
    },
}

impl Located {
    fn origin(&self) -> Origin {
        match self {
            Located::Override { path, .. } => Origin::Override(path.clone()),
            Located::Bif { bif, .. } => Origin::Bif(bif.clone()),
        }
    }
}

/// One resource sitting in the override directory.
struct OverrideEntry {
    /// Resref as named on disk, without extension (original case).
    name: String,
    res_type: u16,
    path: PathBuf,
}

/// An index of the override directory. Files are named `<resref>.<ext>`; one
/// whose extension maps to a known type shadows the same resource in the BIFs.
struct Override {
    /// Whether the `override` directory exists at all (vs. present but empty).
    present: bool,
    entries: Vec<OverrideEntry>,
    /// (UPPERCASE resref, type) -> index into `entries`. First file wins.
    index: HashMap<(String, u16), usize>,
}

impl Override {
    /// Scan `<game_dir>/override`, indexing every file whose extension names a
    /// known resource type. A missing directory yields an empty index.
    fn scan(game_dir: &Path) -> Override {
        let mut entries = Vec::new();
        let mut index = HashMap::new();

        let read_dir = match std::fs::read_dir(game_dir.join("override")) {
            Ok(read_dir) => read_dir,
            Err(_) => {
                return Override {
                    present: false,
                    entries,
                    index,
                };
            }
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // A file is addressable only if its name splits into a resref and an
            // extension that maps to a type; skip anything else.
            let (Some(stem), Some(ext)) = (
                path.file_stem().and_then(OsStr::to_str),
                path.extension().and_then(OsStr::to_str),
            ) else {
                continue;
            };
            let Some(res_type) = extension_type(&ext.to_ascii_lowercase()) else {
                continue;
            };

            index
                .entry((stem.to_ascii_uppercase(), res_type))
                .or_insert(entries.len());
            entries.push(OverrideEntry {
                name: stem.to_string(),
                res_type,
                path,
            });
        }

        Override {
            present: true,
            entries,
            index,
        }
    }

    /// Path of the override file for this name and type, if present.
    fn path(&self, name: &str, res_type: u16) -> Option<&Path> {
        let key = (name.to_ascii_uppercase(), res_type);
        self.index
            .get(&key)
            .and_then(|&i| self.entries.get(i))
            .map(|entry| entry.path.as_path())
    }
}

/// Anything that can go wrong while resolving or reading a resource.
#[derive(Debug)]
pub enum ResourceError {
    Io(std::io::Error),
    Key(KeyError),
    Bif(BifError),
    Tlk(TlkError),
    TwoDA(TwoDAError),
    Eff(EffError),
    Itm(ItmError),
    Spl(SplError),
    Cre(CreError),
    Pro(ProError),
    Sto(StoError),
    Are(AreError),
    Dlg(DlgError),
    Bcs(BcsError),
    /// No resource with this name and type exists in the KEY.
    NotFound {
        name: String,
        res_type: u16,
    },
    /// The KEY locator points at a BIF index that the KEY doesn't list.
    BifIndexOutOfRange(usize),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceError::Io(e) => write!(f, "i/o error: {e}"),
            ResourceError::Key(e) => write!(f, "{e}"),
            ResourceError::Bif(e) => write!(f, "{e}"),
            ResourceError::Tlk(e) => write!(f, "{e}"),
            ResourceError::TwoDA(e) => write!(f, "{e}"),
            ResourceError::Eff(e) => write!(f, "{e}"),
            ResourceError::Itm(e) => write!(f, "{e}"),
            ResourceError::Spl(e) => write!(f, "{e}"),
            ResourceError::Cre(e) => write!(f, "{e}"),
            ResourceError::Pro(e) => write!(f, "{e}"),
            ResourceError::Sto(e) => write!(f, "{e}"),
            ResourceError::Are(e) => write!(f, "{e}"),
            ResourceError::Dlg(e) => write!(f, "{e}"),
            ResourceError::Bcs(e) => write!(f, "{e}"),
            ResourceError::NotFound { name, res_type } => {
                write!(f, "resource not found: {name} (type 0x{res_type:04x})")
            }
            ResourceError::BifIndexOutOfRange(index) => {
                write!(f, "KEY refers to BIF #{index}, which it does not list")
            }
        }
    }
}

impl std::error::Error for ResourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ResourceError::Io(e) => Some(e),
            ResourceError::Key(e) => Some(e),
            ResourceError::Bif(e) => Some(e),
            ResourceError::Tlk(e) => Some(e),
            ResourceError::TwoDA(e) => Some(e),
            ResourceError::Eff(e) => Some(e),
            ResourceError::Itm(e) => Some(e),
            ResourceError::Spl(e) => Some(e),
            ResourceError::Cre(e) => Some(e),
            ResourceError::Pro(e) => Some(e),
            ResourceError::Sto(e) => Some(e),
            ResourceError::Are(e) => Some(e),
            ResourceError::Dlg(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ResourceError {
    fn from(e: std::io::Error) -> Self {
        ResourceError::Io(e)
    }
}

impl From<KeyError> for ResourceError {
    fn from(e: KeyError) -> Self {
        ResourceError::Key(e)
    }
}

impl From<BifError> for ResourceError {
    fn from(e: BifError) -> Self {
        ResourceError::Bif(e)
    }
}

impl From<TlkError> for ResourceError {
    fn from(e: TlkError) -> Self {
        ResourceError::Tlk(e)
    }
}

impl From<TwoDAError> for ResourceError {
    fn from(e: TwoDAError) -> Self {
        ResourceError::TwoDA(e)
    }
}

impl From<EffError> for ResourceError {
    fn from(e: EffError) -> Self {
        ResourceError::Eff(e)
    }
}

impl From<ItmError> for ResourceError {
    fn from(e: ItmError) -> Self {
        ResourceError::Itm(e)
    }
}

impl From<SplError> for ResourceError {
    fn from(e: SplError) -> Self {
        ResourceError::Spl(e)
    }
}

impl From<CreError> for ResourceError {
    fn from(e: CreError) -> Self {
        ResourceError::Cre(e)
    }
}

impl From<ProError> for ResourceError {
    fn from(e: ProError) -> Self {
        ResourceError::Pro(e)
    }
}

impl From<StoError> for ResourceError {
    fn from(e: StoError) -> Self {
        ResourceError::Sto(e)
    }
}

impl From<AreError> for ResourceError {
    fn from(e: AreError) -> Self {
        ResourceError::Are(e)
    }
}

impl From<DlgError> for ResourceError {
    fn from(e: DlgError) -> Self {
        ResourceError::Dlg(e)
    }
}

impl From<BcsError> for ResourceError {
    fn from(e: BcsError) -> Self {
        ResourceError::Bcs(e)
    }
}
