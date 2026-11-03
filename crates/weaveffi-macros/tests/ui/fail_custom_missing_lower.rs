//! A custom type needs a repr and both conversions.

#[weaveffi::custom(repr = String, lift = parse_port)]
pub struct Port(pub u16);

fn parse_port(text: String) -> Result<Port, std::num::ParseIntError> {
    text.parse().map(Port)
}

weaveffi::export_runtime!();

fn main() {}
