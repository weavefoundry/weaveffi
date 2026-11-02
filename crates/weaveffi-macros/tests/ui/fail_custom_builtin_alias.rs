//! A custom type gives a type of its own a repr; an alias of a type that
//! already crosses (`u16` here) would conflict with its built-in crossing.

#[weaveffi::custom(repr = String, lift = parse_port, lower = ToString::to_string)]
pub type Port = u16;

fn parse_port(text: String) -> Result<Port, std::num::ParseIntError> {
    text.parse()
}

weaveffi::export_runtime!();

fn main() {}
