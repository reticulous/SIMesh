//! Radio and device presets + link-budget arithmetic.
//!
//! Data status (v0, 2026-08-30): radio presets mirror the documented
//! Meshtastic modem presets and the MeshCore EU convention used in Berlin;
//! sensitivities are anchored to the Semtech SX1262 datasheet's BW-125 table
//! and scaled analytically with bandwidth (noise floor ∝ BW, i.e.
//! −10·log10(BW/125 kHz); within ~1 dB of the datasheet's other columns).
//! Device antenna gains are PLACEHOLDER typicals (stock whips ≈ 2 dBi) until
//! the community catalog lands — every consumer shows which preset it used,
//! so corrections are data edits, not code changes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadioPreset {
    pub id: &'static str,
    pub freq_mhz: f64,
    pub bw_khz: f64,
    pub spreading_factor: u8,
    /// Regulatory duty-cycle of the sub-band (EU 869.4–869.65: 10%).
    pub duty_cycle_pct: f64,
}

/// Meshtastic EU_868 LongFast — the firmware default (869.525 MHz slot).
pub const MT_EU868_LONG_FAST: RadioPreset = RadioPreset {
    id: "mt-eu868-long-fast",
    freq_mhz: 869.525,
    bw_khz: 250.0,
    spreading_factor: 11,
    duty_cycle_pct: 10.0,
};

/// Meshtastic MediumFast (used by capacity-minded communities).
pub const MT_EU868_MEDIUM_FAST: RadioPreset = RadioPreset {
    id: "mt-eu868-medium-fast",
    freq_mhz: 869.525,
    bw_khz: 250.0,
    spreading_factor: 9,
    duty_cycle_pct: 10.0,
};

/// MeshCore EU, the channel the Berlin community actually operates on.
///
/// CORRECTED. This preset previously carried 869.525 MHz / BW 250 kHz / SF 11
/// -- a byte-for-byte copy of [`MT_EU868_LONG_FAST`] above, under a comment
/// claiming it was the MeshCore shape. Every coverage, gap and siting run made
/// with it was therefore planned against MESHTASTIC's channel.
///
/// The real numbers are measured, not assumed. An RX-only sensing node
/// configured to 869.618 MHz / BW 62.5 kHz / SF 8 / CR 4/8 short-interleaver /
/// sync word 0x12 decoded live Berlin MeshCore traffic for hours, including
/// packets relayed over 37 hops and paths out to 27 km; the same settings are
/// what the community describes as "UK/Narrow", and they match the MeshCore
/// firmware's own defaults (`receive_frequency_hz: 869_618_000`,
/// `bandwidth_hz: 62_500` in that project's app config).
///
/// 869.618 sits in the same 869.4-869.65 MHz sub-band as 869.525, so the 10%
/// duty cycle and the 500 mW ERP ceiling are unchanged and the regulatory
/// ladder below still applies.
///
/// The link budget barely moves -- narrower bandwidth lowers the noise floor
/// by 6 dB while the lower spreading factor gives up 7.5 dB of processing
/// gain, netting about 1.5 dB -- so coverage results computed with the old
/// values are wrong by roughly that much rather than by tens of dB. AIRTIME
/// is the figure that really changes: an SF8/62.5 kHz symbol is 4.10 ms
/// against SF11/250 kHz's 8.19 ms, so anything reasoning about occupancy,
/// duty cycle or carrier-sense neighbours from the old preset was out by 2x.
pub const MC_EU868_DEFAULT: RadioPreset = RadioPreset {
    id: "mc-eu868-default",
    freq_mhz: 869.618,
    bw_khz: 62.5,
    spreading_factor: 8,
    duty_cycle_pct: 10.0,
};

pub const RADIO_PRESETS: &[&RadioPreset] =
    &[&MT_EU868_LONG_FAST, &MT_EU868_MEDIUM_FAST, &MC_EU868_DEFAULT];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevicePreset {
    pub id: &'static str,
    /// Conducted TX power (dBm). SX1262-class nodes top out at +22.
    pub tx_power_dbm: f64,
    /// PLACEHOLDER typical for the stock antenna until the community catalog
    /// provides measured values.
    pub antenna_gain_dbi: f64,
}

pub const GENERIC_SX1262: DevicePreset = DevicePreset {
    id: "generic-sx1262",
    tx_power_dbm: 22.0,
    antenna_gain_dbi: 2.0,
};

pub const DEVICE_PRESETS: &[&DevicePreset] = &[&GENERIC_SX1262];

/// LoRa RX sensitivity (dBm): SX1262 datasheet BW-125 anchors scaled by
/// bandwidth. Valid SF 7..=12.
pub fn sensitivity_dbm(spreading_factor: u8, bw_khz: f64) -> Option<f64> {
    // Semtech DS.SX1261-2 (868 MHz, BW 125 kHz) anchor points.
    let base125 = match spreading_factor {
        7 => -124.0,
        8 => -126.5,
        9 => -129.0,
        10 => -132.0,
        11 => -134.5,
        12 => -137.0,
        _ => return None,
    };
    Some(base125 + 10.0 * (bw_khz / 125.0).log10())
}

/// Maximum tolerable basic transmission loss for a symmetric link between two
/// `device` nodes on `radio`, with a planning fade margin.
pub fn max_path_loss_db(
    device: &DevicePreset,
    radio: &RadioPreset,
    fade_margin_db: f64,
) -> Option<f64> {
    max_path_loss_for(device.tx_power_dbm, device.antenna_gain_dbi, radio, fade_margin_db)
}

/// Link budget for an explicit per-node transmit power — the form the
/// planner uses, because TX power is a PER-SITE decision, not a constant.
///
/// SYMMETRIC: both ends are credited `antenna_gain_dbi`. That is the right
/// shape for "a mesh of identical nodes" and the wrong one the moment a real
/// operator puts a 5.8 dBi collinear on one end and a stock whip on the other,
/// which is why [`max_path_loss_asym`] exists and this delegates to it.
pub fn max_path_loss_for(
    tx_power_dbm: f64,
    antenna_gain_dbi: f64,
    radio: &RadioPreset,
    fade_margin_db: f64,
) -> Option<f64> {
    max_path_loss_asym(tx_power_dbm, antenna_gain_dbi, antenna_gain_dbi, radio, fade_margin_db)
}

/// Link budget with the two ends' antennas given SEPARATELY.
///
/// The budget is a sum of four terms and every one of them is somebody's
/// decision:
///
/// ```text
///   budget = tx_power + tx_gain + rx_gain - sensitivity - fade_margin
/// ```
///
/// Antenna gain enters ONCE PER END, not twice for one end: a link is
/// reciprocal, so the transmitter's gain and the receiver's gain both count,
/// and they are different numbers whenever the two ends are not the same
/// build. The symmetric form hid that behind a single figure, which made the
/// default budget read as "4 dB of antenna" with no way to say whose.
///
/// The default [`GENERIC_SX1262`] gain is a PLACEHOLDER (see the struct
/// field), so any caller that knows its real antennas should pass them:
/// on the Berlin MeshCore preset a 22 dBm node with two 2.0 dBi whips gets
/// 145.5 dB, and swapping one end for a 5.8 dBi collinear gets 149.3 dB —
/// nearly four decibels, which is a different map.
pub fn max_path_loss_asym(
    tx_power_dbm: f64,
    tx_gain_dbi: f64,
    rx_gain_dbi: f64,
    radio: &RadioPreset,
    fade_margin_db: f64,
) -> Option<f64> {
    let sens = sensitivity_dbm(radio.spreading_factor, radio.bw_khz)?;
    Some(tx_power_dbm + tx_gain_dbi + rx_gain_dbi - sens - fade_margin_db)
}

/// Regulatory ceiling (ERP, dBm) for the EU 863–870 MHz sub-band containing
/// `freq_mhz`, per the ERC 70-03 / EN 300 220 SRD allocations most community
/// meshes use. 869.4–869.65 MHz allows 500 mW ERP at 10% duty; the common
/// 868.0–868.6 and 869.7–870.0 slots allow 25 mW / 5 mW ERP.
/// UNVERIFIED against the current national frequency plan — treat as a
/// planning default to be checked before deployment, not legal advice.
pub fn eu_erp_ceiling_dbm(freq_mhz: f64) -> f64 {
    match freq_mhz {
        f if (869.4..=869.65).contains(&f) => 27.0, // 500 mW, 10% duty
        f if (868.0..=868.6).contains(&f) => 14.0,  // 25 mW, 1% duty
        f if (869.7..=870.0).contains(&f) => 7.0,   // 5 mW
        _ => 14.0,
    }
}

/// Selectable transmit powers (dBm) for planning, ascending, capped by the
/// regulatory ceiling of the band. Mesh radios are configured in coarse
/// steps, and running every node at the ceiling is a known way to degrade a
/// CSMA mesh — see the planner's power-reduction pass.
pub fn power_levels_dbm(freq_mhz: f64, device_max_dbm: f64) -> Vec<f64> {
    let ceiling = eu_erp_ceiling_dbm(freq_mhz).min(device_max_dbm);
    [2.0, 5.0, 8.0, 11.0, 14.0, 17.0, 20.0, 22.0, 27.0]
        .into_iter()
        .filter(|&p| p <= ceiling + 1e-9)
        .collect()
}

pub fn find_radio(id: &str) -> Option<&'static RadioPreset> {
    RADIO_PRESETS.iter().copied().find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect: the budget credited `2.0 * antenna_gain_dbi`, one figure for
    /// both ends, so an operator with a 5.8 dBi collinear at their end and a
    /// stock whip at the other could not express it and silently planned with
    /// 2.0 dBi at both. Nearly 4 dB of budget, unstateable.
    #[test]
    fn the_two_ends_antennas_are_separate_inputs() {
        let r = MC_EU868_DEFAULT;
        let sym = max_path_loss_for(22.0, 2.0, &r, 10.0).unwrap();
        let asym = max_path_loss_asym(22.0, 5.8, 2.0, &r, 10.0).unwrap();
        // Only the one end improved, so only that end's gain is added.
        assert!(
            (asym - sym - 3.8).abs() < 1e-9,
            "a 5.8 dBi antenna at ONE end must buy exactly 3.8 dB over a 2.0 dBi one, got {:.3}",
            asym - sym
        );
        // And it is not double counted: both ends upgraded buys twice as much.
        let both = max_path_loss_asym(22.0, 5.8, 5.8, &r, 10.0).unwrap();
        assert!((both - sym - 7.6).abs() < 1e-9, "both ends must buy 7.6 dB, got {:.3}", both - sym);
    }

    /// The symmetric entry point must keep returning exactly what it used to,
    /// because every published coverage number in this repository was computed
    /// through it. 145.5 dB is the documented default budget: a 22 dBm
    /// SX1262 with the placeholder 2.0 dBi whips on the Berlin MeshCore preset
    /// (SF8, 62.5 kHz) and a 10 dB fade margin.
    #[test]
    fn the_symmetric_form_is_unchanged_and_still_reproduces_the_default_budget() {
        let r = MC_EU868_DEFAULT;
        for gain in [0.0, 2.0, 5.8, 9.0] {
            let sym = max_path_loss_for(22.0, gain, &r, 10.0).unwrap();
            let asym = max_path_loss_asym(22.0, gain, gain, &r, 10.0).unwrap();
            assert_eq!(sym.to_bits(), asym.to_bits(), "symmetric != asymmetric at {gain} dBi");
        }
        let default = max_path_loss_db(&GENERIC_SX1262, &r, 10.0).unwrap();
        assert!(
            (default - 145.5).abs() < 0.02,
            "the documented 145.5 dB default must still fall out of the formula, got {default:.3}"
        );
    }

    #[test]
    fn sensitivity_anchors_and_scaling() {
        assert_eq!(sensitivity_dbm(12, 125.0), Some(-137.0));
        // BW 250: +10·log10(2) ≈ +3.01 dB worse.
        let s = sensitivity_dbm(11, 250.0).unwrap();
        assert!((s - (-134.5 + 3.0103)).abs() < 0.01, "{s}");
        assert!(sensitivity_dbm(6, 125.0).is_none());
        // Monotone: higher SF more sensitive; wider BW less sensitive.
        assert!(sensitivity_dbm(12, 125.0) < sensitivity_dbm(7, 125.0));
        assert!(sensitivity_dbm(9, 125.0) < sensitivity_dbm(9, 500.0));
    }

    #[test]
    fn power_levels_respect_the_band_ceiling() {
        // 869.525 (the 500 mW / 10% duty slot) allows up to 27 dBm…
        let hi = power_levels_dbm(869.525, 22.0);
        assert_eq!(*hi.last().unwrap(), 22.0, "device cap binds below 27");
        assert!(hi.contains(&14.0) && hi.contains(&2.0));
        // …while 868.1 is a 25 mW slot.
        let lo = power_levels_dbm(868.1, 22.0);
        assert_eq!(*lo.last().unwrap(), 14.0);
        assert!(!lo.contains(&17.0));
    }

    #[test]
    fn budget_scales_one_for_one_with_power() {
        let a = max_path_loss_for(14.0, 2.0, &MT_EU868_LONG_FAST, 10.0).unwrap();
        let b = max_path_loss_for(22.0, 2.0, &MT_EU868_LONG_FAST, 10.0).unwrap();
        assert!((b - a - 8.0).abs() < 1e-9, "8 dB more power = 8 dB more budget");
    }

    /// The MeshCore preset must not silently be the Meshtastic one.
    ///
    /// It WAS, for the whole life of this file: identical frequency, bandwidth
    /// and spreading factor, under a comment claiming otherwise. Nothing
    /// asserted the values, so every gap census and siting run planned against
    /// the wrong channel and nothing complained.
    ///
    /// The numbers below are what an RX-only sensing node used to decode live
    /// Berlin MeshCore traffic, and what that project's own firmware config
    /// carries. Change them only against a capture, never to make a test pass.
    #[test]
    fn the_meshcore_preset_is_the_channel_meshcore_actually_uses() {
        assert_eq!(MC_EU868_DEFAULT.freq_mhz, 869.618, "Berlin MeshCore RX frequency");
        assert_eq!(MC_EU868_DEFAULT.bw_khz, 62.5, "narrow, not Meshtastic's 250");
        assert_eq!(MC_EU868_DEFAULT.spreading_factor, 8, "SF8, not Meshtastic's SF11");
        // Same 869.4-869.65 sub-band, so the duty cycle and ERP ceiling hold.
        assert_eq!(MC_EU868_DEFAULT.duty_cycle_pct, 10.0);
    }

    /// The specific mistake, stated as its own assertion so that re-introducing
    /// it fails loudly rather than reading as a plausible constant.
    #[test]
    fn the_meshcore_and_meshtastic_presets_are_not_the_same_radio() {
        let (mc, mt) = (&MC_EU868_DEFAULT, &MT_EU868_LONG_FAST);
        assert!(
            mc.freq_mhz != mt.freq_mhz
                || mc.bw_khz != mt.bw_khz
                || mc.spreading_factor != mt.spreading_factor,
            "the MeshCore preset is a copy of the Meshtastic one again"
        );
    }

    #[test]
    fn long_fast_budget_is_plausible() {
        // 22 dBm + 2·2 dBi − (−131.49) − 10 dB margin ≈ 147.5 dB.
        let b = max_path_loss_db(&GENERIC_SX1262, &MT_EU868_LONG_FAST, 10.0).unwrap();
        assert!((b - 147.49).abs() < 0.1, "{b}");
        assert!(find_radio("mc-eu868-default").is_some());
    }
}
