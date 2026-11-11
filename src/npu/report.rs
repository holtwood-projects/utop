//! One-shot output for `utop --once`.

use std::error::Error;
use std::io;

use crate::cli::Config;

use super::collect::{NO_CARD, NpuCollector};
use super::smi;

pub fn once(config: &Config) -> Result<(), Box<dyn Error>> {
    // NO_CARD skips the per-card detail calls; a one-shot sample still
    // gets ECC for the first card plus `/proc` enrichment for every
    // process, which is what scripted checks actually consume.
    let snapshot = NpuCollector::new().sample(NO_CARD);
    if let Some(error) = &snapshot.error {
        if config.json {
            print!("{}", smi::to_json(&snapshot));
        }
        return Err(Box::new(io::Error::other(error.clone())));
    }
    if config.json {
        print!("{}", smi::to_json(&snapshot));
    } else {
        print!("{}", smi::render_text(&snapshot));
    }
    Ok(())
}
