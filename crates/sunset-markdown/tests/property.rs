//! `parse` must be total: never panic on any UTF-8 input.

use proptest::prelude::*;
use sunset_markdown::{parse, to_plain};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn parse_does_not_panic(input in "\\PC*") {
        let _ = parse(&input);
    }

    #[test]
    fn parse_then_plain_does_not_grow(input in "\\PC*") {
        let doc = parse(&input);
        let plain = to_plain(&doc);
        prop_assert!(plain.len() <= input.len());
    }
}
