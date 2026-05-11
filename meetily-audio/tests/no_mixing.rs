//! Compile-fail tests verify the no-mixing rule is enforced at the type
//! level: external code cannot construct a `TranscriptionFrame` from
//! arbitrary samples + label, and cannot call the crate-private
//! constructors directly. Mutation is prevented structurally (private
//! fields, no `&mut` accessors) and is verified by inspection of the API
//! surface in `src/source.rs`.

#[test]
fn no_external_construction_of_transcription_frame() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile-fail/*.rs");
}
