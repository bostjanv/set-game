//! Pure SET rules: cards, set validation, and the table/deck.

use rand::seq::SliceRandom;
use rand::Rng;

/// A card is a number 0..81 whose base-3 digits are its four attributes:
/// count, shape, color, and shading (the client decodes it the same way).
pub type Card = u8;

pub const DECK_SIZE: u8 = 81;
const TABLE_SIZE: usize = 12;

fn attrs(card: Card) -> [u8; 4] {
    [card % 3, (card / 3) % 3, (card / 9) % 3, (card / 27) % 3]
}

/// Three cards form a set when every attribute is all-same or all-different,
/// which for values 0..3 is exactly when the three values sum to a multiple of 3.
pub fn is_set(a: Card, b: Card, c: Card) -> bool {
    if a == b || b == c || a == c || a >= DECK_SIZE || b >= DECK_SIZE || c >= DECK_SIZE {
        return false;
    }
    let (x, y, z) = (attrs(a), attrs(b), attrs(c));
    (0..4).all(|i| (x[i] + y[i] + z[i]) % 3 == 0)
}

pub fn find_set(table: &[Card]) -> Option<[Card; 3]> {
    for i in 0..table.len() {
        for j in i + 1..table.len() {
            for k in j + 1..table.len() {
                if is_set(table[i], table[j], table[k]) {
                    return Some([table[i], table[j], table[k]]);
                }
            }
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    Ok,
    /// The cards are on the table but are not a set: the player is penalized.
    NotASet,
    /// Some card is no longer on the table (another player was faster): no penalty.
    Stale,
}

#[derive(Debug, Clone)]
pub struct Board {
    deck: Vec<Card>,
    table: Vec<Card>,
}

impl Board {
    pub fn new(rng: &mut impl Rng) -> Self {
        let mut deck: Vec<Card> = (0..DECK_SIZE).collect();
        deck.shuffle(rng);
        Self::from_deck(deck)
    }

    /// Builds a board that deals from the end of `deck`.
    pub fn from_deck(deck: Vec<Card>) -> Self {
        let mut board = Board { deck, table: Vec::new() };
        for _ in 0..TABLE_SIZE {
            if let Some(card) = board.deck.pop() {
                board.table.push(card);
            }
        }
        board.ensure_set();
        board
    }

    pub fn table(&self) -> &[Card] {
        &self.table
    }

    pub fn deck_remaining(&self) -> usize {
        self.deck.len()
    }

    pub fn is_over(&self) -> bool {
        self.deck.is_empty() && find_set(&self.table).is_none()
    }

    pub fn claim(&mut self, cards: [Card; 3]) -> Claim {
        if !cards.iter().all(|c| self.table.contains(c)) || cards[0] == cards[1] || cards[1] == cards[2] || cards[0] == cards[2] {
            return Claim::Stale;
        }
        if !is_set(cards[0], cards[1], cards[2]) {
            return Claim::NotASet;
        }
        if self.table.len() <= TABLE_SIZE && self.deck.len() >= 3 {
            // Replace the claimed cards in place so the rest of the layout stays put.
            for slot in self.table.iter_mut() {
                if cards.contains(slot) {
                    *slot = self.deck.pop().expect("deck has 3 cards");
                }
            }
        } else {
            // Shrink by 3, moving surviving cards from the end into the holes.
            let new_len = self.table.len() - 3;
            let mut tail: Vec<Card> =
                self.table[new_len..].iter().copied().filter(|c| !cards.contains(c)).collect();
            self.table.truncate(new_len);
            for slot in self.table.iter_mut() {
                if cards.contains(slot) {
                    *slot = tail.pop().expect("one tail card per hole");
                }
            }
        }
        self.ensure_set();
        Claim::Ok
    }

    /// Deals 3 more cards while the table has no set and the deck has cards.
    fn ensure_set(&mut self) {
        while find_set(&self.table).is_none() && !self.deck.is_empty() {
            for _ in 0..3 {
                if let Some(card) = self.deck.pop() {
                    self.table.push(card);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    // card = count + 3*shape + 9*color + 27*shading
    fn card(count: u8, shape: u8, color: u8, shading: u8) -> Card {
        count + 3 * shape + 9 * color + 27 * shading
    }

    #[test]
    fn validates_sets() {
        // All attributes different.
        assert!(is_set(card(0, 0, 0, 0), card(1, 1, 1, 1), card(2, 2, 2, 2)));
        // Mixed: same count and color, different shape and shading.
        assert!(is_set(card(1, 0, 2, 0), card(1, 1, 2, 2), card(1, 2, 2, 1)));
        // Two share the shape but the third differs: not a set.
        assert!(!is_set(card(0, 0, 0, 0), card(1, 0, 1, 1), card(2, 1, 2, 2)));
        // Two of one color and one of another, everything else fine.
        assert!(!is_set(card(0, 0, 0, 0), card(1, 1, 0, 1), card(2, 2, 1, 2)));
        // Repeated card.
        assert!(!is_set(5, 5, 5));
    }

    #[test]
    fn every_pair_has_exactly_one_completing_card() {
        for a in 0..DECK_SIZE {
            for b in a + 1..DECK_SIZE {
                let n = (0..DECK_SIZE).filter(|&c| is_set(a, b, c)).count();
                assert_eq!(n, 1, "pair {a},{b}");
            }
        }
    }

    #[test]
    fn deals_extra_cards_when_no_set() {
        // 12 cards with no set: colors and shadings only take values 0 and 1, so a set would
        // need all-same color and shading, and its (count, shape) pairs all come from
        // {(0,0),(0,1),(1,0),(1,1)}, which has no three in a line.
        let cap = [(0, 0), (0, 1), (1, 0), (1, 1)];
        let mut no_set: Vec<Card> = Vec::new();
        for (count, shape) in cap {
            no_set.push(card(count, shape, 0, 0));
            no_set.push(card(count, shape, 1, 0));
        }
        for (count, shape) in [(0, 0), (1, 1)] {
            no_set.push(card(count, shape, 0, 1));
            no_set.push(card(count, shape, 1, 1));
        }
        assert_eq!(no_set.len(), 12);
        assert!(find_set(&no_set).is_none());

        let rest: Vec<Card> = (0..DECK_SIZE).filter(|c| !no_set.contains(c)).collect();
        let mut deck = rest;
        deck.extend(no_set.iter().rev()); // dealt first from the end
        let board = Board::from_deck(deck);
        assert!(board.table().len() > 12);
        assert!(board.table().len() % 3 == 0);
        assert!(find_set(board.table()).is_some());
    }

    #[test]
    fn claim_outcomes() {
        let mut board = Board::new(&mut StdRng::seed_from_u64(1));
        let set = find_set(board.table()).unwrap();
        let not_on_table = (0..DECK_SIZE).find(|c| !board.table().contains(c)).unwrap();
        assert_eq!(board.claim([set[0], set[1], not_on_table]), Claim::Stale);
        let t = board.table().to_vec();
        let bad = (0..t.len())
            .flat_map(|i| (i + 1..t.len()).map(move |j| (i, j)))
            .find_map(|(i, j)| t.iter().find(|&&k| k != t[i] && k != t[j] && !is_set(t[i], t[j], k)).map(|&k| [t[i], t[j], k]))
            .unwrap();
        assert_eq!(board.claim(bad), Claim::NotASet);
        assert_eq!(board.table(), &t[..]);

        let before = board.table().to_vec();
        assert_eq!(board.claim(set), Claim::Ok);
        assert_eq!(board.claim(set), Claim::Stale);
        // Unclaimed cards keep their positions when the table had 12 cards.
        if before.len() == 12 {
            for (i, c) in before.iter().enumerate() {
                if !set.contains(c) {
                    assert_eq!(board.table()[i], *c);
                }
            }
        }
    }

    #[test]
    fn random_games_play_to_the_end_without_losing_cards() {
        for seed in 0..200 {
            let mut rng = StdRng::seed_from_u64(seed);
            let mut board = Board::new(&mut rng);
            let mut claimed = 0;
            while !board.is_over() {
                let table = board.table();
                assert!(table.len() <= 21, "seed {seed}: table too large");
                assert_eq!(table.len() + board.deck_remaining() + claimed, 81);
                let mut sorted = table.to_vec();
                sorted.sort();
                sorted.dedup();
                assert_eq!(sorted.len(), table.len(), "duplicate card on table");
                let set = find_set(table).expect("a set exists until the game is over");
                assert_eq!(board.claim(set), Claim::Ok);
                claimed += 3;
            }
            assert_eq!(board.deck_remaining(), 0);
            assert_eq!(board.table().len() + claimed, 81);
        }
    }
}
