use crate::config::{Appearance, Paths};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub version: u32,
    pub name: String,
    pub colors: Colors,
    pub metrics: Metrics,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    pub background: String,
    pub surface: String,
    pub raised: String,
    pub border: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
    pub focus: String,
    pub danger: String,
    pub success: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metrics {
    pub radius: u16,
    pub spacing: u16,
    pub row_height: u16,
}

#[derive(Clone, Debug, Default)]
pub struct Fonts {
    pub interface: Option<String>,
    pub mono: Option<String>,
    pub weight: Option<u16>,
    pub size: Option<u16>,
}

impl Theme {
    pub fn load(name: &str) -> Result<Self, String> {
        let text = if name == "nord" {
            include_str!("../themes/nord.yaml").into()
        } else {
            std::fs::read_to_string(name).map_err(|e| format!("Cannot read theme {name}: {e}"))?
        };
        let theme: Self =
            serde_yaml_ng::from_str(&text).map_err(|e| format!("Invalid theme: {e}"))?;
        let c = &theme.colors;
        if theme.version != 1 {
            return Err("Unsupported theme version; expected 1".into());
        }
        for color in [
            &c.background,
            &c.surface,
            &c.raised,
            &c.border,
            &c.text,
            &c.muted,
            &c.accent,
            &c.focus,
            &c.danger,
            &c.success,
        ] {
            if color.len() != 7
                || !color.starts_with('#')
                || !color[1..].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(format!("Invalid theme color {color:?}; use #rrggbb"));
            }
        }
        let m = &theme.metrics;
        if m.radius > 40 || !(4..=32).contains(&m.spacing) || !(28..=80).contains(&m.row_height) {
            return Err(
                "Theme metrics out of range (radius 0–40, spacing 4–32, row_height 28–80)".into(),
            );
        }
        Ok(theme)
    }

    pub fn css(&self, fonts: &Fonts) -> String {
        let c = &self.colors;
        let m = &self.metrics;
        let mut css = include_str!("../themes/base.css").to_string();
        for (key, value) in [
            ("background", &c.background),
            ("surface", &c.surface),
            ("raised", &c.raised),
            ("border", &c.border),
            ("text", &c.text),
            ("muted", &c.muted),
            ("accent", &c.accent),
            ("focus", &c.focus),
            ("danger", &c.danger),
            ("success", &c.success),
        ] {
            css = css.replace(&format!("@{key}@"), value);
        }
        for (key, value) in [
            ("radius", m.radius),
            ("spacing", m.spacing),
            ("row_height", m.row_height),
        ] {
            css = css.replace(&format!("@{key}@"), &value.to_string());
        }
        if let Some(font) = &fonts.interface {
            css.push_str(&format!(
                "\nwindow {{ font-family: {}; }}",
                css_string(font)
            ));
        }
        if let Some(font) = &fonts.mono {
            css.push_str(&format!("\n.path {{ font-family: {}; }}", css_string(font)));
        }
        if let Some(weight) = fonts.weight {
            css.push_str(&format!(
                "\nwindow {{ font-weight: {}; }}",
                weight.clamp(100, 900)
            ));
        }
        if let Some(size) = fonts.size {
            css.push_str(&format!("\nwindow {{ font-size: {size}pt; }}"));
        }
        css
    }
}

fn css_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\n', '\r'], " ")
    )
}

pub fn detect_fonts(appearance: &Appearance, paths: &Paths) -> Fonts {
    let mut fonts = Fonts::default();
    if appearance.follow_dank
        && let Some(text) = ["dms/settings.json", "DankMaterialShell/settings.json"]
            .iter()
            .find_map(|name| std::fs::read_to_string(paths.config_home.join(name)).ok())
        && let Ok(settings) = serde_json::from_str::<serde_json::Value>(&text)
    {
        fonts.interface = settings["fontFamily"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        fonts.mono = settings["monoFontFamily"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        fonts.weight = settings["fontWeight"]
            .as_u64()
            .map(|n| n.clamp(100, 900) as u16);
    }
    if appearance.font.is_some() {
        fonts.interface.clone_from(&appearance.font);
        fonts.weight = None;
    }
    if appearance.monospace_font.is_some() {
        fonts.mono.clone_from(&appearance.monospace_font);
    }
    fonts.size = appearance.font_size;
    fonts
}
