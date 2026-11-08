#[test]
fn contract_fixture_matches_sibling_crate() {
    let sibling = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../refineable/tests/support/contract.rs");
    match std::fs::read(&sibling) {
        Ok(contents) => assert_eq!(
            include_bytes!("support/contract.rs").as_slice(),
            contents.as_slice(),
            "refinement contract fixtures must remain byte-for-byte identical",
        ),
        // Independently packaged crates do not contain the sibling's fixture.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("failed to read {}: {error}", sibling.display()),
    }
}
