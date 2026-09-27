use planner_core::model::{LinkParams, Loss, ModelError, PathLossModel};
use planner_core::profile::Profile;

/// Free-space path loss — exact, terrain-blind. The optimistic floor no real
/// link beats; used for wiring, tests, and "physics floor" display.
pub struct FreeSpace;

impl PathLossModel for FreeSpace {
    fn id(&self) -> &'static str {
        "free-space"
    }

    fn basic_transmission_loss(
        &self,
        profile: &Profile,
        params: &LinkParams,
    ) -> Result<Loss, ModelError> {
        let d_km = profile.length_m() / 1000.0;
        if d_km <= 0.0 {
            return Err(ModelError::BadProfile("zero-length path".into()));
        }
        if params.freq_mhz <= 0.0 {
            return Err(ModelError::OutOfRange("freq_mhz must be positive".into()));
        }
        let lb_db = 32.447_78 + 20.0 * params.freq_mhz.log10() + 20.0 * d_km.log10();
        Ok(Loss { lb_db })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use planner_core::profile::{ClutterClass, ProfilePoint, Zone};

    fn straight_profile(d_m: f64) -> Profile {
        let mk = |d| ProfilePoint {
            d_m: d,
            h_terrain_m: 0.0,
            h_clutter_m: 0.0,
            clutter: ClutterClass::Open,
            zone: Zone::Inland,
        };
        Profile { points: vec![mk(0.0), mk(d_m)] }
    }

    #[test]
    fn fspl_868mhz_1km() {
        let mut params = LinkParams::eu868_defaults();
        params.freq_mhz = 868.0;
        let loss = FreeSpace
            .basic_transmission_loss(&straight_profile(1000.0), &params)
            .unwrap();
        // 32.44778 + 20*log10(868) + 0 = 91.218 dB
        assert!((loss.lb_db - 91.218).abs() < 0.01, "got {}", loss.lb_db);
    }

    #[test]
    fn fspl_doubles_distance_plus_6db() {
        let params = LinkParams::eu868_defaults();
        let l1 = FreeSpace
            .basic_transmission_loss(&straight_profile(1000.0), &params)
            .unwrap();
        let l2 = FreeSpace
            .basic_transmission_loss(&straight_profile(2000.0), &params)
            .unwrap();
        assert!((l2.lb_db - l1.lb_db - 6.0206).abs() < 0.01);
    }
}
