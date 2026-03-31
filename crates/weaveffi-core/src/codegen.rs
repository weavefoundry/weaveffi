use anyhow::Result;
use camino::Utf8Path;
use weaveffi_ir::ir::Api;

use crate::config::GeneratorConfig;

pub trait Generator {
    fn name(&self) -> &'static str;
    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()>;

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate(api, out_dir)
    }
}

#[derive(Default)]
pub struct Orchestrator<'a> {
    generators: Vec<&'a dyn Generator>,
}

impl<'a> Orchestrator<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generator(mut self, gen: &'a dyn Generator) -> Self {
        self.generators.push(gen);
        self
    }

    pub fn run(&self, api: &Api, out_dir: &Utf8Path, config: &GeneratorConfig) -> Result<()> {
        for g in &self.generators {
            g.generate_with_config(api, out_dir, config)?;
        }
        Ok(())
    }
}
