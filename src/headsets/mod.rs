//! The headsets FreeSeries knows beyond their audio. One file per headset, one line here.
//! How to write one: `docs/headsets.md`.

use crate::headset::Model;

mod arctis_nova_pro_wireless;

pub static ALL: [&Model; 1] = [&arctis_nova_pro_wireless::MODEL];
