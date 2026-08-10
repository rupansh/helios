use helios_kmd_logic::translation_session as model;
#[test]
fn tmp_size() {
    println!("TranslationSession = {}", core::mem::size_of::<model::TranslationSession>());
    println!("align = {}", core::mem::align_of::<model::TranslationSession>());
}
