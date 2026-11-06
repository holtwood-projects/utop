//! One-shot output for `utop --once`.

use std::error::Error;

use crate::cli::Config;

use super::model::NpuSnapshot;
use super::smi;

pub fn once(config: &Config) -> Result<(), Box<dyn Error>> {
    let snapshot = match smi::collect() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let snapshot = NpuSnapshot::error(err.to_string());
            if config.json {
                print!("{}", smi::to_json(&snapshot));
            }
            return Err(Box::new(err));
        }
    };
    if config.json {
        print!("{}", smi::to_json(&snapshot));
    } else {
        print!("{}", smi::render_text(&snapshot));
    }
    Ok(())
}
