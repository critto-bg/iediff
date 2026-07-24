//! Finding resources.
//!
//! Two kinds of search, both driven by the KEY/override index that
//! [`ResourceManager`] exposes:
//!
//! - **by name** — [`search`] does a case-insensitive prefix match over the
//!   index, for browsing. It reads no resource data.
//! - **by reference** — [`find_strref`] and [`find_resref`] answer the reverse
//!   question, "what points *at* this?", by parsing every candidate resource
//!   and inspecting its typed fields.
//!
//! The reverse search reads and parses the whole install per query, and skips
//! anything that fails rather than aborting the sweep: one corrupt file should
//! not hide every other answer.

use std::collections::HashSet;

use crate::formats::are::{ARE_TYPE, Are};
use crate::formats::bcs::{BCS_TYPE, Script};
use crate::formats::cre::{CRE_TYPE, Cre};
use crate::formats::dlg::{DLG_TYPE, Dlg};
use crate::formats::eff::{EFF_TYPE, Eff};
use crate::formats::ids::IDS_TYPE;
use crate::formats::itm::{ITM_TYPE, Itm};
use crate::formats::pro::{PRO_TYPE, Pro};
use crate::formats::spl::{SPL_TYPE, Spl};
use crate::formats::sto::{STO_TYPE, Sto};
use crate::formats::twoda::TWODA_TYPE;
use crate::formats::{
    BAM_TYPE, BMP_TYPE, MOS_TYPE, MVE_TYPE, VVC_TYPE, WAV_TYPE, WED_TYPE, WMP_TYPE,
};
use crate::resource::{Origin, ResourceManager, Source};
use crate::script::Vocabulary;
use crate::{ProjectileUse, ResrefUse, StrrefUse};

/// The decoded binary formats, and so the resources a reverse search reads.
/// BCS scripts name resources through their calls, so they are scanned too —
/// but only with a [`Vocabulary`] to say what each call's arguments mean.
const SCANNED_TYPES: [u16; 9] = [
    EFF_TYPE, ITM_TYPE, SPL_TYPE, CRE_TYPE, PRO_TYPE, STO_TYPE, ARE_TYPE, DLG_TYPE, BCS_TYPE,
];

/// The types [`find_resref`] can find. A query for anything else has nothing to
/// match, and says so rather than reporting "no references found".
///
/// This is the *target* axis, independent of which formats are scanned as
/// referrers ([`SCANNED_TYPES`]): BCS is here because CRE and ARE name scripts,
/// though we never parse one, and MOS and WMP only because scripts name them.
/// Absent are the types nothing we decode names — TIS, IDS, CHU.
///
/// The formats' `resref_uses` are the source of truth; the tests keep this list
/// and the usage guide in step with them.
pub const REFERENCEABLE_KINDS: [u16; 18] = [
    ITM_TYPE, SPL_TYPE, CRE_TYPE, DLG_TYPE, ARE_TYPE, PRO_TYPE, EFF_TYPE, BCS_TYPE, STO_TYPE,
    TWODA_TYPE, BAM_TYPE, VVC_TYPE, WAV_TYPE, WED_TYPE, BMP_TYPE, MVE_TYPE, MOS_TYPE, WMP_TYPE,
];

/// Text formats with no typed fields, scanned only heuristically. DLG is soft-
/// scanned too, but is already in [`SCANNED_TYPES`] for its typed fields.
const SOFT_TYPES: [u16; 2] = [TWODA_TYPE, IDS_TYPE];

/// One resource matched by a prefix search: its name and type.
pub struct SearchHit {
    pub name: String,
    pub res_type: u16,
}

/// Prefix-search results, grouped by where the resources live. Each group is
/// sorted by name then type.
pub struct SearchResults {
    pub over: Vec<SearchHit>,
    pub biff: Vec<SearchHit>,
}

impl SearchResults {
    /// Total number of matches across both layers.
    pub fn total(&self) -> usize {
        self.over.len() + self.biff.len()
    }
}

/// One resource that references the target, and where: `["item[12]"]`,
/// `["line 6"]`. `soft` marks a heuristic match the consumer must judge.
pub struct RefHit {
    pub name: String,
    pub res_type: u16,
    pub origin: Origin,
    pub fields: Vec<String>,
    pub soft: bool,
}

/// Resources whose name starts with `prefix`, optionally filtered to a single
/// type, grouped by override vs biff. `--biff` (`source`) hides the override
/// layer. Each group is sorted by name then type.
pub fn search(
    manager: &ResourceManager,
    prefix: &str,
    type_filter: Option<u16>,
    source: Source,
) -> SearchResults {
    let (over_entries, key_entries) = manager.index_entries();
    let keep = |res_type: u16| type_filter.is_none_or(|t| t == res_type);
    let matching = |entries: &[(&str, u16)]| {
        sorted_hits(
            entries
                .iter()
                .filter(|(name, res_type)| {
                    keep(*res_type) && crate::starts_with_ignore_ascii_case(name, prefix)
                })
                .map(|(name, res_type)| (name.to_string(), *res_type))
                .collect::<Vec<_>>()
                .into_iter(),
        )
    };

    let over = if source == Source::Biff {
        Vec::new()
    } else {
        matching(&over_entries)
    };
    let biff = matching(&key_entries);

    SearchResults { over, biff }
}

/// Find every resource that references strref `target`, matching typed fields
/// and scanning 2DA/IDS text heuristically. `source` and `type_filter` narrow
/// the universe as in [`search`]. Hits are sorted by name then type.
pub fn find_strref(
    manager: &mut ResourceManager,
    target: u32,
    source: Source,
    type_filter: Option<u16>,
) -> Vec<RefHit> {
    let mut types = SCANNED_TYPES.to_vec();
    types.extend_from_slice(&SOFT_TYPES);

    // Built once from the install's IDS tables, so compiled scripts can be
    // typed; `None` leaves them unscanned rather than failing the query.
    let vocabulary = manager.vocabulary();

    // Snapshot the universe first: these names borrow the index, which the
    // read loop below needs `&mut manager` for.
    let (over, key) = manager.index_entries();
    let universe = scan_universe(&over, &key, source, type_filter, &types);
    drop((over, key));

    let mut hits = Vec::new();
    for (name, res_type) in universe {
        let Some((origin, bytes)) = manager.read_for_scan(&name, res_type, source) else {
            continue;
        };

        // Exact matches on typed strref fields, and — for DLG scripts and
        // 2DA/IDS text — heuristic candidates.
        let hard: Vec<String> = extract_strrefs(res_type, &bytes, vocabulary.as_deref())
            .into_iter()
            .filter(|u| u.value == target)
            .map(|u| u.path)
            .collect();
        if !hard.is_empty() {
            hits.push(RefHit {
                name: name.clone(),
                res_type,
                origin: origin.clone(),
                fields: hard,
                soft: false,
            });
        }

        let soft = soft_matches(res_type, &bytes, target);
        if !soft.is_empty() {
            hits.push(RefHit {
                name,
                res_type,
                origin,
                fields: soft,
                soft: true,
            });
        }
    }
    hits.sort_by(|a, b| a.name.cmp(&b.name).then(a.res_type.cmp(&b.res_type)));
    hits
}

/// Find every resource that names `name` as a resource of type `kind`.
///
/// `type_filter` selects the type of the *referring* resources; `kind` is the
/// type of the thing referred to. `name` need not exist — a dangling reference
/// is a legitimate thing to search for. Hits are sorted by name then type.
pub fn find_resref(
    manager: &mut ResourceManager,
    name: &str,
    kind: u16,
    source: Source,
    type_filter: Option<u16>,
) -> Vec<RefHit> {
    // The text formats carry no typed fields, but a resref can appear in their
    // cells, so they join the universe for the heuristic scan.
    let mut types = SCANNED_TYPES.to_vec();
    types.extend_from_slice(&SOFT_TYPES);

    // Built once so compiled scripts and DLG script blocks can be typed.
    let vocabulary = manager.vocabulary();

    // A projectile is named by number, so the query becomes table numbers.
    let projectiles = match kind {
        PRO_TYPE => manager.projectile_numbers(name),
        _ => Vec::new(),
    };

    // Snapshot the universe first: these names borrow the index, which the
    // read loop below needs `&mut manager` for.
    let (over, key) = manager.index_entries();
    let universe = scan_universe(&over, &key, source, type_filter, &types);
    drop((over, key));

    let mut hits = Vec::new();
    for (res_name, res_type) in universe {
        let Some((origin, bytes)) = manager.read_for_scan(&res_name, res_type, source) else {
            continue;
        };

        let (hard, soft) = match_resrefs(
            res_type,
            &bytes,
            name,
            kind,
            vocabulary.as_deref(),
            &projectiles,
        );
        if !hard.is_empty() {
            hits.push(RefHit {
                name: res_name.clone(),
                res_type,
                origin: origin.clone(),
                fields: hard,
                soft: false,
            });
        }
        if !soft.is_empty() {
            hits.push(RefHit {
                name: res_name,
                res_type,
                origin,
                fields: soft,
                soft: true,
            });
        }
    }
    hits.sort_by(|a, b| a.name.cmp(&b.name).then(a.res_type.cmp(&b.res_type)));
    hits
}

/// The `(name, type)` set to scan. Under `Auto` an override entry shadows the
/// KEY entry of the same `(name, type)`; `Biff` ignores the override layer.
/// Duplicates collapse; names compare case-insensitively.
fn scan_universe(
    over: &[(&str, u16)],
    key: &[(&str, u16)],
    source: Source,
    type_filter: Option<u16>,
    types: &[u16],
) -> Vec<(String, u16)> {
    let keep = |t: u16| types.contains(&t) && type_filter.is_none_or(|f| f == t);
    let mut seen: HashSet<(String, u16)> = HashSet::new();
    let mut out = Vec::new();

    let mut push = |name: &str, res_type: u16| {
        if keep(res_type) && seen.insert((name.to_ascii_uppercase(), res_type)) {
            out.push((name.to_string(), res_type));
        }
    };

    if source != Source::Biff {
        for &(name, res_type) in over {
            push(name, res_type);
        }
    }
    for &(name, res_type) in key {
        push(name, res_type);
    }
    out
}

/// Collect (name, type) pairs into [`SearchHit`]s sorted by name then type. The
/// KEY guarantees distinct (name, type) entries, so no deduplication is needed.
fn sorted_hits(items: impl Iterator<Item = (String, u16)>) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = items
        .map(|(name, res_type)| SearchHit { name, res_type })
        .collect();
    hits.sort_by(|a, b| a.name.cmp(&b.name).then(a.res_type.cmp(&b.res_type)));
    hits
}

/// Collect a resource's strref field uses, skipping the name resolution
/// [`decode`](ResourceManager::decode) performs. Bytes that don't parse yield
/// nothing. BCS scripts name strrefs through their calls, so they need the
/// [`Vocabulary`]; the other formats carry them in typed fields.
fn extract_strrefs(res_type: u16, data: &[u8], vocabulary: Option<&Vocabulary>) -> Vec<StrrefUse> {
    match res_type {
        EFF_TYPE => Eff::parse(data)
            .map(|e| e.strref_uses())
            .unwrap_or_default(),
        ITM_TYPE => Itm::parse(data)
            .map(|i| i.strref_uses())
            .unwrap_or_default(),
        SPL_TYPE => Spl::parse(data)
            .map(|s| s.strref_uses())
            .unwrap_or_default(),
        CRE_TYPE => Cre::parse(data)
            .map(|c| c.strref_uses())
            .unwrap_or_default(),
        PRO_TYPE => Pro::parse(data)
            .map(|p| p.strref_uses())
            .unwrap_or_default(),
        STO_TYPE => Sto::parse(data)
            .map(|s| s.strref_uses())
            .unwrap_or_default(),
        ARE_TYPE => Are::parse(data)
            .map(|a| a.strref_uses())
            .unwrap_or_default(),
        DLG_TYPE => Dlg::parse(data)
            .map(|d| {
                let mut uses = d.strref_uses();
                // The action scripts name strrefs by call too (`DisplayString`,
                // journal entries), typed exactly when the vocabulary is present.
                if let Some(vocabulary) = vocabulary {
                    uses.extend(d.script_strref_uses(vocabulary));
                }
                uses
            })
            .unwrap_or_default(),
        BCS_TYPE => match vocabulary {
            Some(vocabulary) => Script::parse(data)
                .map(|s| s.strref_uses(vocabulary))
                .unwrap_or_default(),
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Soft strref matches: 2DA/IDS lines carrying the number. A DLG's script
/// strrefs are typed by the call now, so they are hard and handled in
/// [`extract_strrefs`].
fn soft_matches(res_type: u16, data: &[u8], target: u32) -> Vec<String> {
    match res_type {
        TWODA_TYPE | IDS_TYPE => soft_text_matches(data, target),
        _ => Vec::new(),
    }
}

/// Heuristic ("soft") match for a strref in a text table (2DA/IDS): the 1-based
/// numbers of the lines on which `target` appears as a whitespace-delimited
/// integer token. Decrypts an XOR-encrypted table first. A match is only a
/// candidate — the number could be a strref or any other value.
fn soft_text_matches(data: &[u8], target: u32) -> Vec<String> {
    let text = crate::decrypt_text(data);
    let text = String::from_utf8_lossy(&text);
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            line.split_whitespace()
                .any(|token| token.parse::<u32>() == Ok(target))
        })
        .map(|(i, _)| format!("line {}", i + 1))
        .collect()
}

/// Where `data` names `name` as a resource of type `kind`, as matching field
/// paths split into typed ("hard") and judge-me ("soft").
///
/// `projectiles` holds the `projectl.ids` numbers of the queried `.pro`, empty
/// for any other kind.
///
/// The [`ResrefUse`]s borrow from the parsed resource and die inside each `map`
/// closure; only the matching paths escape. That is what stops a full-install
/// scan cloning every resref in the game.
fn match_resrefs(
    res_type: u16,
    data: &[u8],
    name: &str,
    kind: u16,
    vocabulary: Option<&Vocabulary>,
    projectiles: &[i64],
) -> (Vec<String>, Vec<String>) {
    match res_type {
        EFF_TYPE => Eff::parse(data)
            .map(|r| split_matches(r.resref_uses(), name, kind))
            .unwrap_or_default(),
        ITM_TYPE => Itm::parse(data)
            .map(|r| {
                split_all(
                    r.resref_uses(),
                    r.projectile_uses(),
                    name,
                    kind,
                    projectiles,
                )
            })
            .unwrap_or_default(),
        SPL_TYPE => Spl::parse(data)
            .map(|r| {
                split_all(
                    r.resref_uses(),
                    r.projectile_uses(),
                    name,
                    kind,
                    projectiles,
                )
            })
            .unwrap_or_default(),
        CRE_TYPE => Cre::parse(data)
            .map(|r| split_matches(r.resref_uses(), name, kind))
            .unwrap_or_default(),
        PRO_TYPE => Pro::parse(data)
            .map(|r| {
                split_all(
                    r.resref_uses(),
                    r.projectile_uses(),
                    name,
                    kind,
                    projectiles,
                )
            })
            .unwrap_or_default(),
        STO_TYPE => Sto::parse(data)
            .map(|r| split_matches(r.resref_uses(), name, kind))
            .unwrap_or_default(),
        ARE_TYPE => Are::parse(data)
            .map(|r| split_matches(r.resref_uses(), name, kind))
            .unwrap_or_default(),
        // A dialogue names resources in two ways: `next_dlg` is a typed field,
        // and its trigger and action scripts name them by call — the latter
        // compiled and typed exactly, so those are hard too.
        DLG_TYPE => Dlg::parse(data)
            .map(|r| {
                let (mut hard, soft) = split_matches(r.resref_uses(), name, kind);
                if let Some(vocabulary) = vocabulary {
                    let (script_hard, _) =
                        split_matches(r.script_references(vocabulary), name, kind);
                    hard.extend(script_hard);
                }
                (hard, soft)
            })
            .unwrap_or_default(),
        // A compiled script names resources through its calls' arguments, typed
        // by the same vocabulary — but exactly, from the parsed record rather
        // than from text, so these are hard matches.
        BCS_TYPE => match vocabulary {
            Some(vocabulary) => Script::parse(data)
                .map(|r| split_matches(r.references(vocabulary), name, kind))
                .unwrap_or_default(),
            None => (Vec::new(), Vec::new()),
        },
        // Text tables have no typed fields at all, so every match is a
        // candidate — and an untyped one, which is why `kind` is not consulted:
        // a bare token in a 2DA says nothing about what type it names.
        TWODA_TYPE | IDS_TYPE => (Vec::new(), soft_text_resref_matches(data, name)),
        _ => (Vec::new(), Vec::new()),
    }
}

/// Split the uses naming `name` as a `kind` into hard and soft paths. Resrefs
/// are ASCII and case-insensitive.
fn split_matches(uses: Vec<ResrefUse<'_>>, name: &str, kind: u16) -> (Vec<String>, Vec<String>) {
    let mut hard = Vec::new();
    let mut soft = Vec::new();
    for u in uses {
        if u.kind != kind || !u.value.eq_ignore_ascii_case(name) {
            continue;
        }
        if u.soft { &mut soft } else { &mut hard }.push(u.path);
    }
    (hard, soft)
}

/// [`split_matches`] for a format that also names projectiles by number. Those
/// are hard matches too: a `projectl.ids` number identifies a `.pro` as exactly
/// as a resref would.
fn split_all(
    uses: Vec<ResrefUse<'_>>,
    projectiles: Vec<ProjectileUse>,
    name: &str,
    kind: u16,
    numbers: &[i64],
) -> (Vec<String>, Vec<String>) {
    let (mut hard, soft) = split_matches(uses, name, kind);
    hard.extend(
        projectiles
            .into_iter()
            .filter(|u| numbers.contains(&u.number))
            .map(|u| u.path),
    );
    (hard, soft)
}

/// Soft resref match in a 2DA/IDS table: the 1-based lines where `target` is a
/// whitespace-delimited token, case-insensitively and ignoring a `.ext` suffix.
///
/// Row labels count — `ITEMEXCL.2DA` keys rows *by* item resref. Whole tokens
/// only, since one resref is so often the prefix of another.
fn soft_text_resref_matches(data: &[u8], target: &str) -> Vec<String> {
    let text = crate::decrypt_text(data);
    let text = String::from_utf8_lossy(&text);
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            line.split_whitespace().any(|token| {
                let token = token.split_once('.').map_or(token, |(stem, _)| stem);
                token.eq_ignore_ascii_case(target)
            })
        })
        .map(|(i, _)| format!("line {}", i + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut u: Vec<(String, u16)>) -> Vec<(String, u16)> {
        u.sort();
        u
    }

    #[test]
    fn universe_shadows_key_with_override_under_auto() {
        let over = [("FOO", ITM_TYPE), ("NEW", CRE_TYPE)];
        let key = [("FOO", ITM_TYPE), ("BAR", SPL_TYPE)];
        assert_eq!(
            sorted(scan_universe(
                &over,
                &key,
                Source::Auto,
                None,
                &SCANNED_TYPES
            )),
            vec![
                ("BAR".to_string(), SPL_TYPE),
                ("FOO".to_string(), ITM_TYPE),
                ("NEW".to_string(), CRE_TYPE),
            ]
        );
    }

    #[test]
    fn universe_ignores_override_under_biff() {
        let over = [("NEW", CRE_TYPE)];
        let key = [("FOO", ITM_TYPE)];
        assert_eq!(
            scan_universe(&over, &key, Source::Biff, None, &SCANNED_TYPES),
            vec![("FOO".to_string(), ITM_TYPE)]
        );
    }

    #[test]
    fn universe_dedupes_case_insensitively() {
        let over: [(&str, u16); 0] = [];
        let key = [("foo", ITM_TYPE), ("FOO", ITM_TYPE)];
        assert_eq!(
            scan_universe(&over, &key, Source::Auto, None, &SCANNED_TYPES),
            vec![("foo".to_string(), ITM_TYPE)]
        );
    }

    #[test]
    fn universe_skips_non_strref_types_and_honours_filter() {
        let over: [(&str, u16); 0] = [];
        let key = [("TBL", TWODA_TYPE), ("FOO", ITM_TYPE), ("BAR", SPL_TYPE)];
        // 2DA is excluded from the typed set even with no filter.
        assert_eq!(
            sorted(scan_universe(
                &over,
                &key,
                Source::Auto,
                None,
                &SCANNED_TYPES
            )),
            vec![("BAR".to_string(), SPL_TYPE), ("FOO".to_string(), ITM_TYPE)]
        );
        // A type filter narrows to the one type.
        assert_eq!(
            scan_universe(&over, &key, Source::Auto, Some(ITM_TYPE), &SCANNED_TYPES),
            vec![("FOO".to_string(), ITM_TYPE)]
        );
    }

    #[test]
    fn universe_includes_soft_types_when_requested() {
        let over: [(&str, u16); 0] = [];
        let key = [("TBL", TWODA_TYPE), ("FOO", ITM_TYPE)];
        let mut types = SCANNED_TYPES.to_vec();
        types.extend_from_slice(&SOFT_TYPES);
        assert_eq!(
            sorted(scan_universe(&over, &key, Source::Auto, None, &types)),
            vec![
                ("FOO".to_string(), ITM_TYPE),
                ("TBL".to_string(), TWODA_TYPE)
            ]
        );
    }

    #[test]
    fn soft_text_matches_finds_token_lines() {
        // 2DA-like: sig, default, header, two rows; 25207 is a cell on line 4.
        let text = b"2DA V1.0\n*\nNAME REF\nBERSERKER 25207\nWIZARD_SLAYER 40\n";
        assert_eq!(soft_text_matches(text, 25207), vec!["line 4".to_string()]);
        // The substring 2520 must not match 25207, and a miss yields nothing.
        assert!(soft_text_matches(text, 2520).is_empty());
        assert!(soft_text_matches(text, 99999).is_empty());
    }

    #[test]
    fn soft_text_resref_matches_whole_tokens_only() {
        // A 2DA keyed by resref, as ITEMEXCL.2DA is, with the name also used as
        // a cell value, as RNDTRES.2DA does.
        let table = b"2DA V1.0\n*\nCOL1\nSW1H01 1\nROW2 SW1H01\nROW3 SW1H01A\nROW4 sw1h01.itm\n";
        assert_eq!(
            soft_text_resref_matches(table, "SW1H01"),
            vec!["line 4", "line 5", "line 7"]
        );
        // Case-insensitive from the query side too.
        assert_eq!(
            soft_text_resref_matches(table, "sw1h01").len(),
            3,
            "resrefs are ASCII and case-insensitive"
        );
        // A prefix must not match: item names routinely extend one another,
        // which is why line 6 (SW1H01A) is absent above.
        assert!(soft_text_resref_matches(table, "SW1H0").is_empty());
        assert!(soft_text_resref_matches(table, "W1H01").is_empty());
        assert!(soft_text_resref_matches(table, "NOSUCH").is_empty());
    }

    #[test]
    fn split_matches_keys_on_kind_and_ignores_resref_case() {
        let uses = vec![
            ResrefUse::new(ITM_TYPE, "SW1H01", "item[0]"),
            // Same name, different type: a SPL named SW1H01 is not the item.
            ResrefUse::new(SPL_TYPE, "SW1H01", "known_spell[0]"),
            // Resrefs are ASCII and case-insensitive, so this is the same item.
            ResrefUse::new(ITM_TYPE, "sw1h01", "item[1]"),
            // A near miss must not match on a prefix.
            ResrefUse::new(ITM_TYPE, "SW1H011", "item[2]"),
            ResrefUse::new(ITM_TYPE, "SW1H01", "op318.resource").soft(),
        ];
        let (hard, soft) = split_matches(uses, "sW1h01", ITM_TYPE);
        assert_eq!(hard, vec!["item[0]".to_string(), "item[1]".to_string()]);
        assert_eq!(soft, vec!["op318.resource".to_string()]);
    }

    // ---- end-to-end, against the synthetic game in `fixture` ----------------
    //
    // These drive the whole pipeline — index, override shadowing, BIF reads,
    // per-format parsing, hard/soft classification — against a tree whose exact
    // contents are known, so they can assert answers rather than properties.
    // See `fixture::build` for the layout.

    use crate::fixture;
    use crate::resource::ResourceManager;

    /// `(name.ext, fields)` for each hit, so assertions read like the CLI does.
    fn hits(hits: &[RefHit], soft: bool) -> Vec<(String, Vec<String>)> {
        hits.iter()
            .filter(|h| h.soft == soft)
            .map(|h| {
                (
                    format!(
                        "{}.{}",
                        h.name,
                        crate::formats::key::type_extension(h.res_type)
                    ),
                    h.fields.clone(),
                )
            })
            .collect()
    }

    fn fixture_manager(game: &fixture::FixtureGame) -> ResourceManager {
        ResourceManager::open(&game.key_path()).expect("open fixture game")
    }

    #[test]
    fn a_projectile_is_found_through_the_number_that_names_it() {
        // The table ties the two together, and gives PRO_TARGET two numbers.
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        assert_eq!(
            m.projectile_numbers(fixture::PRO_TARGET),
            vec![
                i64::from(fixture::PROJECTILE_NUMBER),
                i64::from(fixture::PROJECTILE_NUMBER) + 40
            ]
        );

        let found = find_resref(&mut m, fixture::PRO_TARGET, PRO_TYPE, Source::Auto, None);
        let hit = found
            .iter()
            .find(|h| h.name == "MAGIC")
            .expect("the spell fires this projectile");
        assert!(!hit.soft, "a table number identifies the file exactly");
        assert_eq!(hit.fields, vec!["ability[0].projectile"]);

        // A projectile the table does not name is not reachable at all.
        let none = find_resref(&mut m, "NOSUCHPR", PRO_TYPE, Source::Auto, None);
        assert!(none.iter().all(|h| h.name != "MAGIC"));
    }

    #[test]
    fn a_compiled_script_names_a_strref_through_its_calls() {
        // `DisplayString(Myself,12345)` in the BCS witness: the strref rides an
        // integer argument, typed by the call, and so is a hard match.
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let found = find_strref(&mut m, fixture::SCRIPT_STRREF, Source::Auto, None);
        let hit = found
            .iter()
            .find(|h| h.name == "AISCRIPT")
            .expect("the script displays this strref");
        assert!(!hit.soft, "a strref typed by the call is a hard match");
        assert_eq!(hit.fields, vec!["block[0].DisplayString.StrRef"]);
    }

    #[test]
    fn finds_every_referrer_across_both_layers() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let found = find_resref(&mut m, "SW1H01", ITM_TYPE, Source::Auto, None);

        assert_eq!(
            hits(&found, false),
            vec![
                // A compiled script naming the item in a trigger — hard, from
                // the parsed record, not text.
                (
                    "AISCRIPT.bcs".into(),
                    vec!["block[0].HasItem.ResRef".to_string()]
                ),
                // Archived only.
                ("ARCHIVED.cre".into(), vec!["item[0]".to_string()]),
                // Override-only: exists in no BIF and has no KEY entry.
                ("LOOSE.sto".into(), vec!["sale[0]".to_string()]),
                // The typed effect, reached through the spell's casting pool.
                (
                    "MAGIC.spl".into(),
                    vec!["casting[0].op122.resource".to_string()]
                ),
                // Two different sections of one area, in section order.
                (
                    "MAP.are".into(),
                    vec!["item[0]".to_string(), "door[0].key_item".to_string()]
                ),
                // The override copy wins, and it names the item twice.
                (
                    "SHADOWED.cre".into(),
                    vec!["item[0]".to_string(), "item[1]".to_string()]
                ),
                // A standalone effect: no `effect[n]` prefix, it *is* the effect.
                ("SINGLE.eff".into(), vec!["op122.resource".to_string()]),
                // An item may name itself: SW1H01 degrades into SW1H01.
                ("SW1H01.itm".into(), vec!["replacement_item".to_string()]),
                // A dialogue's scripts name the item by call — a trigger and the
                // action inside its `ActionOverride` — now compiled and typed,
                // so hard rather than heuristic.
                (
                    "TALK.dlg".into(),
                    vec![
                        "state_trigger[0].PartyHasItem.Item".to_string(),
                        "action[0].GiveItemCreate.ResRef".to_string(),
                    ],
                ),
            ]
        );
        // DECOY.itm names nothing, so it must not appear at all.
        assert!(found.iter().all(|h| h.name != "DECOY"));
    }

    #[test]
    fn every_soft_source_is_reported_separately_from_typed_hits() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let found = find_resref(&mut m, fixture::ITM_TARGET, ITM_TYPE, Source::Auto, None);

        assert_eq!(
            hits(&found, true),
            vec![
                // A text table: the resref as a row label (line 4) and as a
                // cell (line 5). Line 6 holds `SW1H01A` and must not match — a
                // resref is very often the prefix of another.
                (
                    "LOOT.2da".into(),
                    vec!["line 4".to_string(), "line 5".to_string()]
                ),
                // #318 takes "the spell or item specified in the Resource
                // field", so an item reading is a candidate, not a fact.
                (
                    "MAGIC.spl".into(),
                    vec!["casting[1].op318.resource".to_string()]
                ),
                // TALK.dlg's script references are hard now (compiled and typed),
                // so they no longer appear among the soft candidates.
            ]
        );
    }

    #[test]
    fn biff_source_ignores_the_override_layer() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let found = find_resref(&mut m, "SW1H01", ITM_TYPE, Source::Biff, None);
        let names: Vec<String> = hits(&found, false).into_iter().map(|(n, _)| n).collect();

        // LOOSE.sto is override-only, so `--biff` cannot see it...
        assert_eq!(
            names,
            vec![
                "AISCRIPT.bcs",
                "ARCHIVED.cre",
                "MAGIC.spl",
                "MAP.are",
                "SHADOWED.cre",
                "SINGLE.eff",
                "SW1H01.itm",
                "TALK.dlg",
            ]
        );
        // ...and SHADOWED falls back to the archived body, which names the item
        // once rather than twice.
        let shadowed = hits(&found, false)
            .into_iter()
            .find(|(n, _)| n == "SHADOWED.cre")
            .expect("archived copy");
        assert_eq!(shadowed.1, vec!["item[0]"]);
    }

    #[test]
    fn type_filter_narrows_the_referrers_not_the_target() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let found = find_resref(&mut m, "SW1H01", ITM_TYPE, Source::Auto, Some(CRE_TYPE));
        let names: Vec<String> = hits(&found, false).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["ARCHIVED.cre", "SHADOWED.cre"]);
    }

    #[test]
    fn the_target_is_matched_by_kind_not_just_by_name() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let as_spell = find_resref(&mut m, "SW1H01", SPL_TYPE, Source::Auto, None);

        // Every *typed* reference in the fixture names SW1H01 as an item, so
        // asking for a spell of that name finds none of them: the inventories,
        // the sale slot and the #122 effect are all item-typed.
        assert!(
            hits(&as_spell, false).is_empty(),
            "no typed field names SW1H01 as a spell"
        );

        // The soft answers split three ways. #318 offers both readings, so a
        // spell search legitimately surfaces MAGIC.spl. The 2DA still matches,
        // because a bare token in a table types nothing and cannot be filtered.
        // But TALK.dlg drops out: its calls are `PartyHasItem` and
        // `GiveItemCreate`, which the script vocabulary knows take an *item*,
        // so they cannot answer a search for a spell of the same name.
        assert_eq!(
            hits(&as_spell, true)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            vec!["LOOT.2da", "MAGIC.spl"]
        );
    }

    #[test]
    fn resref_matching_ignores_case_and_rejects_prefixes() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        let upper = find_resref(&mut m, "SW1H01", ITM_TYPE, Source::Auto, None).len();
        let lower = find_resref(&mut m, "sw1h01", ITM_TYPE, Source::Auto, None).len();
        assert_eq!(upper, lower);
        assert!(upper > 0);
        // A prefix of a real resref is not a match.
        assert!(find_resref(&mut m, "SW1H0", ITM_TYPE, Source::Auto, None).is_empty());
    }

    #[test]
    fn a_target_that_exists_nowhere_yields_nothing() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        // Searching for a dangling reference is legitimate, and simply empty
        // here — it must not error or match something adjacent.
        assert!(find_resref(&mut m, "NOSUCH", ITM_TYPE, Source::Auto, None).is_empty());
    }

    #[test]
    fn prefix_search_groups_override_and_archived_separately() {
        let game = fixture::build();
        let m = fixture_manager(&game);
        let results = search(&m, "s", None, Source::Auto);
        let names = |hits: &[SearchHit]| -> Vec<String> {
            hits.iter()
                .map(|h| {
                    format!(
                        "{}.{}",
                        h.name,
                        crate::formats::key::type_extension(h.res_type)
                    )
                })
                .collect()
        };
        assert_eq!(names(&results.over), vec!["SHADOWED.cre"]);
        assert_eq!(
            names(&results.biff),
            vec!["SHADOWED.cre", "SINGLE.eff", "SW1H01.itm"]
        );
        // The shadowed creature is listed under both layers, by design: the
        // grouping shows where each copy lives, not which one would win.
        assert_eq!(results.total(), 4);
    }

    // ---- coverage invariants over the fixture --------------------------------
    //
    // The tests above assert particular answers. These assert that the *set* of
    // things the search can answer is complete: every format we scan yields
    // references, and every kind we claim to find has a witness. They are what
    // keeps the tables in this module honest as formats are added — wire a new
    // format into `resref_uses` but forget `match_resrefs`, or add a kind to
    // `REFERENCEABLE_KINDS` with nothing producing it, and these fail.

    /// Every reference the fixture contains: `(referrer, kind, target, path)`.
    /// Hand-written rather than derived from `resref_uses`, deliberately — a
    /// table generated from the code under test could only agree with itself.
    const DECLARED: &[(&str, u16, &str, &str)] = &[
        // ITM header fields.
        ("SW1H01", ITM_TYPE, fixture::ITM_TARGET, "replacement_item"),
        ("SW1H01", BAM_TYPE, fixture::BAM_TARGET, "inventory_icon"),
        // CRE header and sections.
        ("ARCHIVED", BMP_TYPE, fixture::BMP_TARGET, "small_portrait"),
        ("ARCHIVED", BCS_TYPE, fixture::BCS_TARGET, "script_override"),
        ("ARCHIVED", DLG_TYPE, fixture::DLG_TARGET, "dialog"),
        ("ARCHIVED", SPL_TYPE, fixture::SPL_TARGET, "known_spell[0]"),
        ("ARCHIVED", ITM_TYPE, fixture::ITM_TARGET, "item[0]"),
        // SPL header, then one effect per resource-bearing opcode family.
        ("MAGIC", WAV_TYPE, fixture::WAV_TARGET, "completion_sound"),
        (
            "MAGIC",
            ITM_TYPE,
            fixture::ITM_TARGET,
            "casting[0].op122.resource",
        ),
        (
            "MAGIC",
            SPL_TYPE,
            fixture::SPL_TARGET,
            "casting[2].op146.resource",
        ),
        (
            "MAGIC",
            CRE_TYPE,
            fixture::CRE_TARGET,
            "casting[3].op67.resource",
        ),
        (
            "MAGIC",
            EFF_TYPE,
            fixture::EFF_TARGET,
            "casting[4].op177.resource",
        ),
        (
            "MAGIC",
            TWODA_TYPE,
            fixture::TWODA_TARGET,
            "casting[5].op214.resource",
        ),
        (
            "MAGIC",
            MVE_TYPE,
            fixture::MVE_TARGET,
            "casting[6].op152.resource",
        ),
        (
            "MAGIC",
            BMP_TYPE,
            fixture::BMP_TARGET,
            "casting[7].op107.resource",
        ),
        (
            "MAGIC",
            VVC_TYPE,
            fixture::OVERLAY_TARGET,
            "casting[8].op215.resource",
        ),
        (
            "MAGIC",
            BCS_TYPE,
            fixture::BCS_TARGET,
            "casting[9].op82.resource",
        ),
        (
            "MAGIC",
            ARE_TYPE,
            fixture::ARE_TARGET,
            "casting[10].op186.resource",
        ),
        // Named by projectl.ids number, not by resref.
        (
            "MAGIC",
            PRO_TYPE,
            fixture::PRO_TARGET,
            "ability[0].projectile",
        ),
        // ARE header and sections.
        ("MAP", WED_TYPE, fixture::WED_TARGET, "wed"),
        ("MAP", ARE_TYPE, fixture::ARE_TARGET, "north"),
        ("MAP", BCS_TYPE, fixture::BCS_TARGET, "script"),
        ("MAP", CRE_TYPE, fixture::CRE_TARGET, "actor[0].cre"),
        ("MAP", DLG_TYPE, fixture::DLG_TARGET, "actor[0].dialog"),
        ("MAP", ITM_TYPE, fixture::ITM_TARGET, "item[0]"),
        ("MAP", ITM_TYPE, fixture::ITM_TARGET, "door[0].key_item"),
        ("MAP", WAV_TYPE, fixture::WAV_TARGET, "ambient[0].sound[0]"),
        (
            "MAP",
            BAM_TYPE,
            fixture::BAM_TARGET,
            "animation[0].resource",
        ),
        (
            "MAP",
            PRO_TYPE,
            fixture::PRO_TARGET,
            "proj_trap[0].projectile",
        ),
        // PRO.
        ("BOLT", WAV_TYPE, fixture::WAV_TARGET, "fire_sound"),
        ("BOLT", SPL_TYPE, fixture::SPL_TARGET, "default_spell"),
        (
            "BOLT",
            BAM_TYPE,
            fixture::BAM_TARGET,
            "travel.projectile_animation",
        ),
        ("BOLT", BMP_TYPE, fixture::BMP_TARGET, "travel.palette"),
        // STO (override-only), DLG, standalone EFF.
        ("LOOSE", DLG_TYPE, fixture::DLG_TARGET, "rumours_tavern"),
        ("LOOSE", ITM_TYPE, fixture::ITM_TARGET, "sale[0]"),
        ("LOOSE", SPL_TYPE, fixture::SPL_TARGET, "cure[0]"),
        (
            "TALK",
            DLG_TYPE,
            fixture::DLG_TARGET,
            "transition[0].next_dlg",
        ),
        ("SINGLE", ITM_TYPE, fixture::ITM_TARGET, "op122.resource"),
        // BCS: a trigger's string, an action's strings, and a spell by number.
        (
            "AISCRIPT",
            ITM_TYPE,
            fixture::ITM_TARGET,
            "block[0].HasItem.ResRef",
        ),
        (
            "AISCRIPT",
            CRE_TYPE,
            fixture::CRE_TARGET,
            "block[0].CreateCreature.NewObject",
        ),
        (
            "AISCRIPT",
            BCS_TYPE,
            fixture::BCS_TARGET,
            "block[0].ChangeAIScript.ScriptFile",
        ),
        (
            "AISCRIPT",
            SPL_TYPE,
            fixture::SPELL_BY_NUMBER,
            "block[0].ForceSpell.Spell",
        ),
        // The three types nothing but a script names.
        (
            "AISCRIPT",
            STO_TYPE,
            fixture::STO_TARGET,
            "block[0].StartStore.Store",
        ),
        (
            "AISCRIPT",
            ARE_TYPE,
            fixture::ARE_TARGET,
            "block[0].LeaveAreaLUA.Area",
        ),
        (
            "AISCRIPT",
            MOS_TYPE,
            fixture::MOS_TARGET,
            "block[0].LeaveAreaLUA.Parchment",
        ),
        (
            "AISCRIPT",
            WMP_TYPE,
            fixture::WMP_TARGET,
            "block[0].SetWorldmap.Worldmap",
        ),
    ];

    #[test]
    fn every_declared_reference_is_found_by_a_reverse_search() {
        let game = fixture::build();
        let mut m = fixture_manager(&game);
        for &(referrer, kind, target, path) in DECLARED {
            let found = find_resref(&mut m, target, kind, Source::Auto, None);
            let hit = found
                .iter()
                .find(|h| h.name == referrer)
                .unwrap_or_else(|| {
                    panic!(
                        "{referrer} should reference {target} (type 0x{kind:04x}) at {path}, \
                     but the search returned {:?}",
                        found.iter().map(|h| &h.name).collect::<Vec<_>>()
                    )
                });
            assert!(
                hit.fields.iter().any(|f| f == path),
                "{referrer} -> {target}: expected field {path}, got {:?}",
                hit.fields
            );
        }
    }

    #[test]
    fn every_scanned_format_contributes_references() {
        // A format listed in SCANNED_TYPES but absent from `match_resrefs`
        // would silently contribute nothing to every search.
        let game = fixture::build();
        let manager = fixture_manager(&game);
        let (over, key) = manager.index_entries();
        let present: HashSet<u16> = over.iter().chain(key.iter()).map(|&(_, t)| t).collect();
        for res_type in SCANNED_TYPES {
            assert!(
                present.contains(&res_type),
                "fixture has no resource of scanned type 0x{res_type:04x}"
            );
            assert!(
                DECLARED.iter().any(|&(name, ..)| {
                    over.iter()
                        .chain(key.iter())
                        .any(|&(n, t)| t == res_type && n.eq_ignore_ascii_case(name))
                }),
                "no fixture resource of type 0x{res_type:04x} declares a reference"
            );
        }
    }

    #[test]
    fn every_referenceable_kind_has_a_witness() {
        // A kind in REFERENCEABLE_KINDS that nothing can actually produce would
        // make `--uses` accept the query and then always answer "none".
        for kind in REFERENCEABLE_KINDS {
            assert!(
                DECLARED.iter().any(|&(_, k, ..)| k == kind),
                "no fixture reference names type 0x{kind:04x}, \
                 yet REFERENCEABLE_KINDS claims it is searchable"
            );
        }
    }
}
