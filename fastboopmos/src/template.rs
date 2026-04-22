use anyhow::{Context, Result};
use minijinja::{Environment, UndefinedBehavior, Value, context};

use crate::index::RootfsSelection;

pub fn render_manifest(
    template_text: &str,
    release_name: &str,
    selection: &RootfsSelection,
) -> Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_keep_trailing_newline(true);

    let variant_value: Value = match &selection.variant {
        Some(v) => Value::from(v.clone()),
        None => Value::from(()),
    };

    let ctx = context! {
        release_name => release_name,
        pmos_device => selection.pmos_device.as_str(),
        ui_name => selection.ui_name.as_str(),
        variant => variant_value,
        target_name => selection.target_name(),
        image_name => selection.image_name.as_str(),
        image_url => selection.image_url.as_str(),
        image_sha512 => selection.image_sha512.as_str(),
        image_size => selection.image_size,
        timestamp => selection.timestamp.as_str(),
    };

    let rendered = env
        .render_str(template_text, ctx)
        .context("rendering template")?;
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(variant: Option<&str>) -> RootfsSelection {
        RootfsSelection {
            pmos_device: "oneplus-fajita".into(),
            ui_name: "plasma-mobile".into(),
            variant: variant.map(String::from),
            image_name: "img.img.xz".into(),
            image_url: "https://example.invalid/img.img.xz".into(),
            image_sha512: "deadbeef".into(),
            image_size: 12345,
            timestamp: "20260101-000000".into(),
        }
    }

    #[test]
    fn renders_target_name_bare() {
        let out = render_manifest("id: {{ target_name }}\n", "edge", &sample(None)).unwrap();
        assert_eq!(out, "id: oneplus-fajita\n");
    }

    #[test]
    fn renders_target_name_variant() {
        let out = render_manifest(
            "id: {{ target_name }}\n",
            "edge",
            &sample(Some("factory")),
        )
        .unwrap();
        assert_eq!(out, "id: oneplus-fajita-factory\n");
    }

    #[test]
    fn strict_undefined_errors_on_missing() {
        let err = render_manifest("{{ nope }}", "edge", &sample(None));
        assert!(err.is_err(), "expected error for undefined variable");
    }

    #[test]
    fn keeps_trailing_newline() {
        let out = render_manifest("x\n", "edge", &sample(None)).unwrap();
        assert_eq!(out, "x\n");
    }

    #[test]
    fn renders_all_context_variables() {
        let tmpl = "r={{ release_name }} d={{ pmos_device }} u={{ ui_name }} \
                    t={{ target_name }} n={{ image_name }} url={{ image_url }} \
                    h={{ image_sha512 }} s={{ image_size }} ts={{ timestamp }}\n";
        let out = render_manifest(tmpl, "edge", &sample(None)).unwrap();
        assert_eq!(
            out,
            "r=edge d=oneplus-fajita u=plasma-mobile t=oneplus-fajita \
             n=img.img.xz url=https://example.invalid/img.img.xz \
             h=deadbeef s=12345 ts=20260101-000000\n"
        );
    }
}
