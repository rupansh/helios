//! Generate complete typed DDI table literals from bindgen's syntax tree.
//! The emitted literal deliberately has no `..Default`: a parser or header
//! change which omits a field must fail Rust compilation. Bindgen may emit
//! unformatted tokens when rustfmt is absent, so whitespace is never syntax.

const TABLE_NAMES: &[&str] = &[
    "D3D11DDI_DEVICEFUNCS",
    "D3D11_1DDI_DEVICEFUNCS",
    "D3DWDDM1_3DDI_DEVICEFUNCS",
    "DXGI_DDI_BASE_FUNCTIONS",
    "DXGI1_1_DDI_BASE_FUNCTIONS",
    "DXGI1_3_DDI_BASE_FUNCTIONS",
];

fn generate_source(bindings: &str) -> String {
    let parsed = syn::parse_file(bindings).expect("parse target WDK bindings for typed DDI tables");
    let mut generated = String::from("// Generated from target WDK bindings; do not edit.\n");
    for &name in TABLE_NAMES {
        let underscored = format!("_{name}");
        let definition = parsed
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Struct(definition)
                    if definition.ident == name || definition.ident == underscored =>
                {
                    Some(definition)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("target bindings have no complete {name} definition"));
        let syn::Fields::Named(fields) = &definition.fields else {
            panic!("target DDI table {name} does not have named fields");
        };
        assert!(
            !fields.named.is_empty(),
            "target DDI table {name} has no fields"
        );
        generated.push_str(&format!("impl DdiStubTable for ddi::{name} {{\n fn stubbed<const KIND: usize>() -> Self {{\n Self {{\n"));
        for field in &fields.named {
            let field = field
                .ident
                .as_ref()
                .expect("named DDI field has no identifier");
            generated.push_str(&format!(" {field}: helios_umd_common::noop::TypedStub::<FallbackReport<KIND>>::typed_stub(),\n"));
        }
        generated.push_str(" }\n }\n}\n");
    }
    generated
}

pub fn generate(bindings: &str, output: &std::path::Path) {
    std::fs::write(
        output.join("d3d11_typed_tables.rs"),
        generate_source(bindings),
    )
    .expect("write typed D3D11 DDI initializers");
}

#[cfg(test)]
mod tests {
    use super::{generate_source, TABLE_NAMES};

    #[test]
    fn formatted_and_unformatted_bindings_preserve_every_field() {
        let mut formatted = String::new();
        let mut unformatted = String::new();
        for name in TABLE_NAMES {
            formatted.push_str(&format!(
                "pub struct {name} {{\n    pub first: Option<unsafe extern \"system\" fn(u32, u64)>,\n    pub last: Option<unsafe extern \"system\" fn() -> usize>,\n}}\n"
            ));
            unformatted.push_str(&format!(
                "pub struct {name}{{pub first:Option<unsafe extern \"system\" fn(u32,u64)>,pub last:Option<unsafe extern \"system\" fn()->usize>,}}"
            ));
        }
        let formatted_output = generate_source(&formatted);
        assert_eq!(formatted_output, generate_source(&unformatted));
        assert_eq!(
            formatted_output.matches(" first:").count(),
            TABLE_NAMES.len()
        );
        assert_eq!(
            formatted_output.matches(" last:").count(),
            TABLE_NAMES.len()
        );
    }

    #[test]
    #[should_panic(expected = "no complete D3D11DDI_DEVICEFUNCS definition")]
    fn missing_table_refuses_generation() {
        generate_source("pub struct Unrelated { pub first: usize }");
    }
}
