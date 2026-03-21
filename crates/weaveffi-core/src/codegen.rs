use anyhow::Result;
use camino::Utf8Path;
use weaveffi_ir::ir::Api;

pub trait Generator {
    fn name(&self) -> &'static str;
    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()>;
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

    pub fn run(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        for g in &self.generators {
            g.generate(api, out_dir)?;
        }
        Ok(())
    }
}
