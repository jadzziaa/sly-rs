//! Function names known for each game, from `games/<game>/symbols.tsv`
//! (address, mangled name, readable name; `#` lines are comments).

use crate::Game;

/// A named function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Symbol {
    /// Start address.
    pub address: u32,
    /// The name as the toolchain mangled it.
    pub mangled: &'static str,
    /// A readable form (`Class::method`).
    pub readable: &'static str,
}

const SLY1: &str = include_str!("../../../games/sly1/symbols.tsv");

/// The known function symbols of `game`, by address. Sly 1's come from its
/// decompilation; Sly 2 and 3 have none yet.
#[must_use]
pub fn symbols(game: Game) -> Vec<Symbol> {
    let text = match game {
        Game::Sly1 => SLY1,
        Game::Sly2 | Game::Sly3 => "",
    };
    text.lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let address = u32::from_str_radix(fields.next()?, 16).ok()?;
            let mangled = fields.next()?;
            let readable = fields.next().unwrap_or(mangled);
            Some(Symbol {
                address,
                mangled,
                readable,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sly1_symbols_parse() {
        let symbols = symbols(Game::Sly1);
        assert!(symbols.len() > 3900);
        assert!(symbols.windows(2).all(|w| w[0].address < w[1].address));
        assert_eq!(symbols[0].mangled, "_start");
        assert_eq!(symbols[0].address, 0x0010_0008);
    }
}
