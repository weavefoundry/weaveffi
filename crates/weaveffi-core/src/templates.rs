use anyhow::{Context, Result};
use camino::Utf8Path;
use tera::Tera;

pub struct TemplateEngine {
    tera: Tera,
}

impl TemplateEngine {
    pub fn new() -> Self {
        Self {
            tera: Tera::default(),
        }
    }

    pub fn load_builtin(&mut self, name: &str, content: &str) -> Result<()> {
        self.tera
            .add_raw_template(name, content)
            .with_context(|| format!("failed to load builtin template '{name}'"))
    }

    pub fn load_dir(&mut self, dir: &Utf8Path) -> Result<()> {
        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("failed to read template directory '{dir}'"))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "tera") {
                let name = path
                    .file_name()
                    .expect("file entry must have a name")
                    .to_string_lossy();
                let content = std::fs::read_to_string(&path).with_context(|| {
                    format!("failed to read template file '{}'", path.display())
                })?;
                self.tera
                    .add_raw_template(&name, &content)
                    .with_context(|| format!("failed to parse template '{name}'"))?;
            }
        }
        Ok(())
    }

    pub fn render(&self, name: &str, context: &tera::Context) -> Result<String> {
        self.tera
            .render(name, context)
            .with_context(|| format!("failed to render template '{name}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_render_basic() {
        let mut engine = TemplateEngine::new();
        engine.load_builtin("greeting", "hello {{ name }}").unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("name", "world");

        let output = engine.render("greeting", &ctx).unwrap();
        assert_eq!(output, "hello world");
    }

    #[test]
    fn load_dir_overrides_builtin() {
        let mut engine = TemplateEngine::new();
        engine
            .load_builtin("test.tera", "original {{ val }}")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(dir_path.join("test.tera"), "override {{ val }}").unwrap();

        engine.load_dir(dir_path).unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("val", "ok");
        let output = engine.render("test.tera", &ctx).unwrap();
        assert_eq!(output, "override ok");
    }
}
