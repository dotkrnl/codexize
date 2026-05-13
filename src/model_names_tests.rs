use super::*;

#[test]
fn every_baked_model_has_curated_display_vendor() {
    for row in crate::logic::selection::baked::BAKED_TABLE {
        assert_ne!(
            display_vendor(row.model),
            None,
            "baked model {} must have a curated display vendor",
            row.model
        );
    }
}
