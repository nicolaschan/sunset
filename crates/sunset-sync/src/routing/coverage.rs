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
        Filter::Specific(super_vk, super_name) => {
            covers_specific(super_vk, super_name, subset)
        }
        Filter::Keyspace(super_vk) => covers_keyspace(super_vk, subset),
        Filter::Namespace(super_name) => covers_namespace(super_name, subset),
        // Other superset variants implemented in subsequent tasks.
        _ => unimplemented!("covers: superset variant not yet implemented"),
    }
}

/// `Specific(super_vk, super_name)` covers `subset` iff `subset` matches
/// exactly that one key — i.e. `subset` is itself `Specific(super_vk,
/// super_name)`, or a `Union` whose every alternative is covered by it.
fn covers_specific(super_vk: &VerifyingKey, super_name: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(sub_vk, sub_name) => super_vk == sub_vk && super_name == sub_name,
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::Specific(super_vk.clone(), super_name.clone()), alt)
        }),
        Filter::Keyspace(_) | Filter::Namespace(_) | Filter::NamePrefix(_) => false,
    }
}

/// `Keyspace(super_vk)` covers `subset` iff every match of `subset` is
/// written by `super_vk`.
fn covers_keyspace(super_vk: &VerifyingKey, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(sub_vk, _) => super_vk == sub_vk,
        Filter::Keyspace(sub_vk) => super_vk == sub_vk,
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::Keyspace(super_vk.clone()), alt)
        }),
        Filter::Namespace(_) | Filter::NamePrefix(_) => false,
    }
}

/// `Namespace(super_name)` covers `subset` iff every match of `subset` has
/// `name == super_name`.
fn covers_namespace(super_name: &Bytes, subset: &Filter) -> bool {
    match subset {
        Filter::Specific(_, sub_name) => super_name == sub_name,
        Filter::Namespace(sub_name) => super_name == sub_name,
        Filter::Union(alts) => alts.iter().all(|alt| {
            covers(&Filter::Namespace(super_name.clone()), alt)
        }),
        Filter::Keyspace(_) | Filter::NamePrefix(_) => false,
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
        assert!(covers(&s, &Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k1")),
            Filter::Specific(vk(b"a"), n(b"k2")),
        ])));
        assert!(!covers(&s, &Filter::Union(vec![
            Filter::Specific(vk(b"a"), n(b"k1")),
            Filter::Specific(vk(b"b"), n(b"k1")),
        ])));
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
}
