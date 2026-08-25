//! The catalogue: what is for sale, and at what price.
//!
//! # Why this is data and not a port
//!
//! `migo-media` takes object storage as a port because storage is an external system with
//! credentials and failure modes; `migo-moderation` takes the staff roster as a port
//! because who is staff is an authorisation fact that lives elsewhere. A catalogue is
//! neither. It is a table of prices — the kind of thing a deployment sets once and changes
//! rarely — so it lives here as an in-memory map, seeded with section 28's ten gifts and
//! extended by the composition root with whatever avatar items, stickers, and themes that
//! deployment sells.
//!
//! Keeping it in the crate rather than the store is deliberate: a price is not
//! per-account state, it is the same for everyone, and reading it should not be a database
//! round trip on the hot path of every purchase. The store records what was *bought*
//! (`entitlement`, `gift_sent`, and the ledger); the catalogue records only what *could*
//! be, and it is rebuilt from configuration on every boot.

use std::collections::BTreeMap;

use crate::model::{Attributes, Category, Gift, Listing, Price, Sku};

/// The default price of a gift, in coins.
///
/// A gentle curve: a rose is cheap enough to send on a whim, a dragon costs real
/// engagement, and Mystery sits in the middle as an affordable surprise. These are
/// defaults a deployment overrides; the shape is what matters, not the exact numbers.
const fn default_gift_price(gift: Gift) -> i64 {
    match gift {
        Gift::Rose => 10,
        Gift::Heart => 20,
        Gift::Cake => 50,
        Gift::Star => 100,
        Gift::Fire => 150,
        Gift::Rocket => 300,
        Gift::Crown => 500,
        Gift::Diamond => 1_000,
        Gift::Dragon => 2_000,
        Gift::Mystery => 250,
    }
}

/// The reputation a gift confers on its recipient, in points.
///
/// A tenth of the price, floored at one. Sending an expensive gift says more than sending a
/// cheap one, so it confers more standing — but the ratio is fixed and small, because
/// reputation is section 87's non-cash-outable currency and letting it track price too
/// closely would make it a shadow balance.
const fn default_gift_reputation(gift: Gift) -> i64 {
    let rep = default_gift_price(gift) / 10;
    if rep < 1 {
        1
    } else {
        rep
    }
}

/// The default attributes of a gift.
const fn default_gift_attributes(gift: Gift) -> Attributes {
    match gift {
        Gift::Dragon => Attributes::ANIMATED
            .with(Attributes::RARE)
            .with(Attributes::COLLECTIBLE),
        Gift::Diamond => Attributes::RARE.with(Attributes::COLLECTIBLE),
        Gift::Crown => Attributes::COLLECTIBLE,
        Gift::Rocket | Gift::Fire => Attributes::ANIMATED,
        Gift::Mystery => Attributes::LIMITED,
        _ => Attributes::none(),
    }
}

/// A price list, keyed by the catalogue code.
///
/// Ordered (`BTreeMap`) so that listing the whole catalogue is deterministic — a shop that
/// renders its items in a different order on every request is a shop that looks broken.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    listings: BTreeMap<String, Listing>,
}

impl Catalogue {
    /// A catalogue with nothing in it.
    ///
    /// A deployment that sells only the standard gifts uses [`Catalogue::with_default_gifts`]
    /// instead; this is for one that prices everything itself.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A catalogue seeded with section 28's ten gifts at their default prices.
    ///
    /// The starting point for most deployments: the gifts are named in the brief and every
    /// deployment has them, so priming them here saves every composition root from
    /// re-listing the same ten. Additional items are added with [`Catalogue::list`].
    ///
    /// # Panics
    ///
    /// Never in practice: every slug in [`Gift::ALL`] is a valid SKU by construction, so the
    /// `expect` documents an invariant of the built-in gift set, not a runtime failure path.
    #[must_use]
    pub fn with_default_gifts() -> Self {
        let mut catalogue = Self::empty();
        for gift in Gift::ALL {
            let sku = Sku::new(Category::Gift, gift.slug()).expect("gift slug is a valid sku");
            catalogue.list(Listing {
                sku,
                price: Price::coins(default_gift_price(gift)),
                attributes: default_gift_attributes(gift),
                reputation: default_gift_reputation(gift),
            });
        }
        catalogue
    }

    /// Adds or replaces a listing.
    ///
    /// Replacing rather than refusing a duplicate, because configuration is declarative: a
    /// deployment that re-prices the dragon writes the dragon again, and the last word
    /// wins. Returns `self` so a builder can chain.
    pub fn list(&mut self, listing: Listing) -> &mut Self {
        self.listings.insert(listing.sku.code(), listing);
        self
    }

    /// The listing for a code, if it is sold.
    #[must_use]
    pub fn get(&self, sku: &Sku) -> Option<&Listing> {
        self.listings.get(&sku.code())
    }

    /// Whether a code is sold.
    #[must_use]
    pub fn contains(&self, sku: &Sku) -> bool {
        self.listings.contains_key(&sku.code())
    }

    /// Every listing, in code order.
    #[must_use]
    pub fn all(&self) -> Vec<&Listing> {
        self.listings.values().collect()
    }

    /// Every listing in one category, in code order.
    #[must_use]
    pub fn in_category(&self, category: Category) -> Vec<&Listing> {
        self.listings
            .values()
            .filter(|listing| listing.sku.category() == category)
            .collect()
    }

    /// How many listings there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.listings.len()
    }

    /// Whether the catalogue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.listings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gifts_are_all_present_and_priced() {
        let catalogue = Catalogue::with_default_gifts();
        assert_eq!(catalogue.len(), Gift::ALL.len());
        for gift in Gift::ALL {
            let sku = Sku::parse(&gift.code()).expect("gift code parses");
            let listing = catalogue.get(&sku).expect("gift is listed");
            assert!(listing.price.amount > 0, "every gift has a positive price");
            assert!(listing.reputation >= 1, "every gift confers some reputation");
        }
    }

    #[test]
    fn mystery_is_a_fixed_price_listing_like_any_other() {
        // Section 87: Mystery must be a fixed-price surprise, not a wager. The catalogue
        // proves it structurally — Mystery has exactly one price and one reputation value,
        // the same as a rose, with nothing random anywhere near it.
        let catalogue = Catalogue::with_default_gifts();
        let sku = Sku::parse("gift.mystery").expect("valid");
        let a = catalogue.get(&sku).expect("listed").clone();
        let b = catalogue.get(&sku).expect("listed").clone();
        assert_eq!(a.price, b.price, "the price never varies between reads");
        assert_eq!(a.reputation, b.reputation);
    }

    #[test]
    fn listing_replaces_rather_than_duplicates() {
        let mut catalogue = Catalogue::with_default_gifts();
        let before = catalogue.len();
        let sku = Sku::parse("gift.rose").expect("valid");
        catalogue.list(Listing {
            sku: sku.clone(),
            price: Price::gems(3),
            attributes: Attributes::none(),
            reputation: 1,
        });
        assert_eq!(catalogue.len(), before, "re-pricing does not add a row");
        assert_eq!(catalogue.get(&sku).expect("listed").price, Price::gems(3));
    }

    #[test]
    fn extending_with_a_theme_keeps_it_separate_from_gifts() {
        let mut catalogue = Catalogue::with_default_gifts();
        let theme = Sku::parse("theme.midnight").expect("valid");
        catalogue.list(Listing {
            sku: theme.clone(),
            price: Price::gems(20),
            attributes: Attributes::none(),
            reputation: 0,
        });
        assert!(catalogue.contains(&theme));
        assert_eq!(catalogue.in_category(Category::Gift).len(), Gift::ALL.len());
        assert_eq!(catalogue.in_category(Category::Theme).len(), 1);
    }
}
