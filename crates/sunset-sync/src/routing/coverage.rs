//! Pure predicate: does one filter cover (is a superset of) another?
//!
//! Used by the receiver ranking to ask "if a candidate already
//! subscribes to filter S, will my filter F be satisfied for free?"
//! Answer: yes iff every `(vk, name)` matching F also matches S, i.e.
//! `covers(S, F) == true`.

use bytes::Bytes;
use sunset_store::{Filter, VerifyingKey};

/// True iff every `(vk, name)` matching `subset` also matches `superset`.
///
/// Equivalent to "subscribing to `superset` would deliver everything
/// `subset` asks for." The relation is reflexive, transitive, and
/// not symmetric.
pub fn covers(superset: &Filter, subset: &Filter) -> bool {
    match superset {
        Filter::Specific(super_vk, super_name) => covers_specific(super_vk, super_name, subset),
        Filter::Keyspace(super_vk) => covers_keyspace(super_vk, subset),
        Filter::Namespace(super_name) => covers_namespace(super_name, subset),
        Filter::NamePrefix(super_prefix) => covers_name_prefix(super_prefix, subset),
        Filter::Union(super_alts) => covers_union(super_alts, subset),
    }
}

/// `Specific(super_vk, super_name)` covers `subset` iff `subset` matches
/// exactly that one key — i.e. `subset` is itself `Specific(super_vk,
/// super_name)`, or a `Union` whose every alternative is covered by it.
fn covers_specific(super_vk: &VerifyingKey, super_name: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(sub_vk, sub_name) => super_vk == sub_vk && super_name == sub_name,
        Filter::Union(alts) => alts
            .iter()
            .all(|alt| covers(&Filter::Specific(super_vk.clone(), super_name.clone()), alt)),
        Filter::Keyspace(_) | Filter::Namespace(_) | Filter::NamePrefix(_) => false,
    }
}

/// `Keyspace(super_vk)` covers `subset` iff every match of `subset` is
/// written by `super_vk`.
fn covers_keyspace(super_vk: &VerifyingKey, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(sub_vk, _) => super_vk == sub_vk,
        Filter::Keyspace(sub_vk) => super_vk == sub_vk,
        Filter::Union(alts) => alts
            .iter()
            .all(|alt| covers(&Filter::Keyspace(super_vk.clone()), alt)),
        Filter::Namespace(_) | Filter::NamePrefix(_) => false,
    }
}

/// `Namespace(super_name)` covers `subset` iff every match of `subset` has
/// `name == super_name`.
fn covers_namespace(super_name: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(_, sub_name) => super_name == sub_name,
        Filter::Namespace(sub_name) => super_name == sub_name,
        Filter::Union(alts) => alts
            .iter()
            .all(|alt| covers(&Filter::Namespace(super_name.clone()), alt)),
        Filter::Keyspace(_) | Filter::NamePrefix(_) => false,
    }
}

/// `NamePrefix(super_prefix)` covers `subset` iff every match of `subset`
/// has a name starting with `super_prefix`.
fn covers_name_prefix(super_prefix: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(_, sub_name) => sub_name.starts_with(super_prefix.as_ref()),
        Filter::Namespace(sub_name) => sub_name.starts_with(super_prefix.as_ref()),
        Filter::NamePrefix(sub_prefix) => sub_prefix.starts_with(super_prefix.as_ref()),
        Filter::Union(alts) => alts
            .iter()
            .all(|alt| covers(&Filter::NamePrefix(super_prefix.clone()), alt)),
        Filter::Keyspace(_) => false,
    }
}

/// `Union(super_alts)` covers `subset` iff:
/// - `subset` is itself a `Union(sub_alts)` and every `sub_alt` is
///   covered by at least one `super_alt`, OR
/// - `subset` is a non-Union and some `super_alt` covers it.
fn covers_union(super_alts: &[Filter], subset: &Filter) -> bool {
    match subset {
        Filter::Union(sub_alts) => sub_alts
            .iter()
            .all(|sub_alt| super_alts.iter().any(|sup| covers(sup, sub_alt))),
        _ => super_alts.iter().any(|sup| covers(sup, subset)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(seed: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(seed))
    }

    fn n(b: &'static [u8]) -> Bytes {
        Bytes::from_static(b)
    }

    #[test]
    fn specific_covers_itself() {
        let f = Filter::Specific(vk(b"a"), n(b"k"));
        assert!(covers(&f, &f));
    }

    #[test]
    fn specific_does_not_cover_different_specific() {
        let s = Filter::Specific(vk(b"a"), n(b"k"));
        assert!(!covers(&s, &Filter::Specific(vk(b"b"), n(b"k"))));
        assert!(!covers(&s, &Filter::Specific(vk(b"a"), n(b"other"))));
    }

    #[test]
    fn specific_does_not_cover_broader_filters() {
        let s = Filter::Specific(vk(b"a"), n(b"k"));
        assert!(!covers(&s, &Filter::Keyspace(vk(b"a"))));
        assert!(!covers(&s, &Filter::Namespace(n(b"k"))));
        assert!(!covers(&s, &Filter::NamePrefix(n(b""))));
    }

    #[test]
    fn specific_covers_union_of_only_itself() {
        let s = Filter::Specific(vk(b"a"), n(b"k"));
        let single = Filter::Union(vec![Filter::Specific(vk(b"a"), n(b"k"))]);
        let two_same = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Specific(vk(b"a"), n(b"k")),
        ]);
        let mixed = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Specific(vk(b"b"), n(b"k")),
        ]);
        assert!(covers(&s, &single));
        assert!(covers(&s, &two_same));
        assert!(!covers(&s, &mixed));
    }

    #[test]
    fn keyspace_covers_itself_and_specific_under_it() {
        let s = Filter::Keyspace(vk(b"a"));
        assert!(covers(&s, &s));
        assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"any"))));
        assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"other"))));
    }

    #[test]
    fn keyspace_does_not_cover_other_writer() {
        let s = Filter::Keyspace(vk(b"a"));
        assert!(!covers(&s, &Filter::Keyspace(vk(b"b"))));
        assert!(!covers(&s, &Filter::Specific(vk(b"b"), n(b"k"))));
    }

    #[test]
    fn keyspace_does_not_cover_writer_agnostic_filters() {
        let s = Filter::Keyspace(vk(b"a"));
        assert!(!covers(&s, &Filter::Namespace(n(b"k"))));
        assert!(!covers(&s, &Filter::NamePrefix(n(b""))));
    }

    #[test]
    fn keyspace_covers_union_iff_all_alts_under_it() {
        let s = Filter::Keyspace(vk(b"a"));
        assert!(covers(
            &s,
            &Filter::Union(vec![
                Filter::Specific(vk(b"a"), n(b"k1")),
                Filter::Specific(vk(b"a"), n(b"k2")),
            ])
        ));
        assert!(!covers(
            &s,
            &Filter::Union(vec![
                Filter::Specific(vk(b"a"), n(b"k1")),
                Filter::Specific(vk(b"b"), n(b"k1")),
            ])
        ));
    }

    #[test]
    fn namespace_covers_itself_and_specifics_with_same_name() {
        let s = Filter::Namespace(n(b"room/x"));
        assert!(covers(&s, &s));
        assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"room/x"))));
        assert!(covers(&s, &Filter::Specific(vk(b"b"), n(b"room/x"))));
    }

    #[test]
    fn namespace_does_not_cover_other_name() {
        let s = Filter::Namespace(n(b"room/x"));
        assert!(!covers(&s, &Filter::Namespace(n(b"room/y"))));
        assert!(!covers(&s, &Filter::Specific(vk(b"a"), n(b"room/y"))));
    }

    #[test]
    fn namespace_does_not_cover_writer_or_prefix_filters() {
        let s = Filter::Namespace(n(b"room/x"));
        assert!(!covers(&s, &Filter::Keyspace(vk(b"a"))));
        // Even NamePrefix matching only the same name string isn't covered —
        // a prefix matches anything-with-that-prefix, not the exact name.
        assert!(!covers(&s, &Filter::NamePrefix(n(b"room/x"))));
    }

    #[test]
    fn name_prefix_covers_specifics_under_prefix() {
        let s = Filter::NamePrefix(n(b"room/"));
        assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"room/general"))));
        assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"room/"))));
        assert!(!covers(&s, &Filter::Specific(vk(b"a"), n(b"other"))));
    }

    #[test]
    fn name_prefix_covers_namespaces_and_longer_prefixes_under_it() {
        let s = Filter::NamePrefix(n(b"room/"));
        assert!(covers(&s, &Filter::Namespace(n(b"room/general"))));
        assert!(covers(&s, &Filter::NamePrefix(n(b"room/x/"))));
        assert!(covers(&s, &Filter::NamePrefix(n(b"room/"))));
        assert!(!covers(&s, &Filter::NamePrefix(n(b"r")))); // shorter, broader
    }

    #[test]
    fn name_prefix_does_not_cover_keyspace() {
        let s = Filter::NamePrefix(n(b"room/"));
        assert!(!covers(&s, &Filter::Keyspace(vk(b"a"))));
    }

    #[test]
    fn empty_prefix_covers_everything_name_based() {
        let s = Filter::NamePrefix(n(b""));
        assert!(covers(&s, &Filter::Specific(vk(b"x"), n(b"anything"))));
        assert!(covers(&s, &Filter::Namespace(n(b"anything"))));
        assert!(covers(&s, &Filter::NamePrefix(n(b"x/"))));
        // Still doesn't cover Keyspace (writer-keyed, not name-keyed).
        assert!(!covers(&s, &Filter::Keyspace(vk(b"x"))));
    }

    #[test]
    fn union_superset_covers_when_any_alt_covers() {
        let s = Filter::Union(vec![
            Filter::Keyspace(vk(b"a")),
            Filter::NamePrefix(n(b"room/")),
        ]);
        assert!(covers(&s, &Filter::Specific(vk(b"a"), n(b"random"))));
        assert!(covers(&s, &Filter::Specific(vk(b"b"), n(b"room/x"))));
        assert!(!covers(&s, &Filter::Specific(vk(b"b"), n(b"other"))));
    }

    #[test]
    fn union_superset_covers_union_subset_pairwise() {
        let s = Filter::Union(vec![
            Filter::Keyspace(vk(b"a")),
            Filter::NamePrefix(n(b"room/")),
        ]);
        let covered = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Namespace(n(b"room/x")),
        ]);
        let not_covered = Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Specific(vk(b"b"), n(b"presence")),
        ]);
        assert!(covers(&s, &covered));
        assert!(!covers(&s, &not_covered));
    }

    #[test]
    fn empty_union_covers_nothing_and_is_covered_by_anything() {
        // Empty Union as superset: no alternative can cover anything, so always false.
        let empty_super = Filter::Union(vec![]);
        assert!(!covers(&empty_super, &Filter::Specific(vk(b"a"), n(b"k"))));

        // Empty Union as subset: vacuous "every alt covered" → true.
        let real_super = Filter::Keyspace(vk(b"a"));
        let empty_sub = Filter::Union(vec![]);
        assert!(covers(&real_super, &empty_sub));
    }

    #[test]
    fn covers_is_reflexive() {
        let filters = [
            Filter::Specific(vk(b"a"), n(b"k")),
            Filter::Keyspace(vk(b"a")),
            Filter::Namespace(n(b"k")),
            Filter::NamePrefix(n(b"k")),
            Filter::Union(vec![
                Filter::Keyspace(vk(b"a")),
                Filter::NamePrefix(n(b"r/")),
            ]),
        ];
        for f in &filters {
            assert!(covers(f, f), "covers({f:?}, {f:?}) should be true");
        }
    }
}
