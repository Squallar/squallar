//! Oracle tests for the Bunkers right-mover against MetPy 1.7.1
//! (`metpy.calc.bunkers_storm_motion`), an independent implementation of the
//! Bunkers et al. 2000 formulation.
//!
//! **Non-circular by construction.** The *inputs* are real VAD wind profiles
//! this crate fitted from Level II volumes in the shared corpus; the
//! *expected values* are MetPy's output on those identical numbers,
//! transcribed from a MetPy run. Nothing below asserts against a rustdar
//! constant — flipping `BUNKERS_DEVIATION_MS`, either band or the
//! right/left sense fails these tests rather than moving them.
//!
//! Six volumes, three used for every judgement and three held out, spanning
//! VCPs 12/21/31/35 and site elevations from 185 m to 883 m.
//!
//! Scope: this pins the *formulation* — the mean-wind layer, the two shear
//! bands, the deviation magnitude and the right/left convention. It says
//! nothing about whether the profile handed in is a good fit. That question
//! has a different oracle (the RPG's own NVW product) and needs the network,
//! so it is measured rather than asserted here.

use rustdar_radar::nrot::WindProfile;
use rustdar_radar::srv::bunkers_right_mover_uv;

/// One fitted profile and what MetPy makes of it.
struct Case {
    /// The volume the profile was fitted from, and whether it was a decision
    /// site or a holdout.
    volume: &'static str,
    /// Layer winds in m/s. Index `l` is the layer centred at
    /// `(l as f64 + 0.5) * WindProfile::LAYER_KM`.
    layers: &'static [Option<(f64, f64)>],
    /// `bunkers_storm_motion(...)[0]`, the right-mover, m/s.
    metpy_right_mover: (f64, f64),
    /// `bunkers_storm_motion(...)[2]`, the 0-6 km mean wind, m/s.
    metpy_mean_wind: (f64, f64),
}

fn profile(layers: &[Option<(f64, f64)>]) -> WindProfile {
    let levels: Vec<(f64, f64, f64)> = layers
        .iter()
        .enumerate()
        .filter_map(|(l, w)| w.map(|(u, v)| ((l as f64 + 0.5) * WindProfile::LAYER_KM, u, v)))
        .collect();
    WindProfile::from_levels(&levels).expect("a full profile builds")
}

/// Metres per second per knot, so the residual is reported in the unit the
/// product displays.
const MS_PER_KNOT: f64 = 0.514_444;

fn knots_apart(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt() / MS_PER_KNOT
}

/// Volumes whose 0-0.5 -> 5.5-6 km shear clears rustdar's floor, so the
/// deviation term applies and the whole formulation is exercised.
const SHEARED: &[Case] = &[
    Case {
        volume: "DMX (decision)",
        layers: &[
            Some((0.89275, 5.94443)),
            Some((4.47513, 9.07625)),
            Some((7.98591, 11.9176)),
            Some((11.9657, 12.9926)),
            Some((14.8732, 12.8349)),
            Some((16.5003, 13.3109)),
            Some((17.3167, 13.5442)),
            Some((17.6471, 14.0812)),
            Some((17.2895, 14.8386)),
            Some((17.647, 15.36)),
            Some((16.7453, 15.6583)),
            Some((16.1368, 16.1195)),
            Some((15.5667, 15.9464)),
            Some((13.9194, 16.3397)),
            Some((12.6425, 16.5308)),
            Some((11.6074, 16.4755)),
            Some((10.2846, 15.6498)),
            Some((10.2117, 16.1213)),
            Some((10.5236, 16.5857)),
            Some((11.1258, 16.8971)),
            Some((9.83672, 15.4122)),
            Some((10.5144, 16.0028)),
            Some((10.1929, 14.6936)),
            Some((9.04207, 14.1073)),
            Some((6.79067, 11.5404)),
            Some((2.90631, 9.89449)),
            Some((-2.25014, 6.33306)),
            Some((1.70103, 6.41194)),
            Some((0.943028, -0.407761)),
            Some((2.31721, 3.48703)),
            Some((-1.91143, -2.70081)),
            Some((-1.56158, -3.49321)),
            Some((8.15661, -4.13293)),
            Some((2.7849, -5.35996)),
            Some((2.7849, -5.35996)),
            Some((26.6908, 1.9504)),
            Some((26.6908, 1.9504)),
            Some((26.6908, 1.9504)),
            Some((26.6908, 1.9504)),
            Some((26.6908, 1.9504)),
        ],
        metpy_right_mover: (18.6021, 9.29167),
        metpy_mean_wind: (12.9179, 14.1845),
    },
    Case {
        volume: "MSX (decision)",
        layers: &[
            Some((1.97345, 4.6879)),
            Some((2.2033, 7.44595)),
            Some((3.22819, 8.40254)),
            Some((5.22541, 9.11598)),
            Some((6.89478, 10.0923)),
            Some((7.76816, 10.8726)),
            Some((8.03984, 11.2921)),
            Some((8.03435, 11.5439)),
            Some((7.93183, 11.7116)),
            Some((7.57192, 11.7788)),
            Some((6.91972, 11.9474)),
            Some((5.89649, 12.043)),
            Some((5.35325, 12.3772)),
            Some((4.59076, 12.6373)),
            Some((4.97669, 13.3282)),
            Some((5.41383, 13.7933)),
            Some((5.904, 13.6682)),
            Some((6.19846, 13.1764)),
            Some((6.82149, 13.0306)),
            Some((7.61538, 12.9788)),
            Some((8.51713, 12.8411)),
            Some((9.28637, 12.797)),
            Some((9.64031, 13.1096)),
            Some((9.53932, 12.6286)),
            Some((9.00275, 11.2568)),
            Some((7.04669, 8.89067)),
            Some((3.06649, 5.13714)),
            Some((1.50632, 2.35453)),
            Some((0.875303, 1.1409)),
            Some((-0.334631, 0.900274)),
            Some((-0.194233, 1.0088)),
            Some((-0.377176, 0.603802)),
            Some((-0.269226, 0.439406)),
            Some((-0.371863, 0.512802)),
            Some((-0.227627, 0.638322)),
            Some((-0.14325, 0.419097)),
            Some((-0.511135, 0.684435)),
            Some((-0.274347, 0.74043)),
            Some((-0.131183, 0.548772)),
            Some((-0.419323, 0.265887)),
        ],
        metpy_right_mover: (11.5918, 6.20368),
        metpy_mean_wind: (5.97956, 11.1789),
    },
    Case {
        volume: "TLX (decision)",
        layers: &[
            Some((-2.62097, -7.26804)),
            Some((-3.17381, -7.15741)),
            Some((-4.41261, -7.22046)),
            Some((-2.24203, -3.34741)),
            Some((1.08152, 1.07535)),
            Some((1.08152, 1.07535)),
            Some((1.08152, 1.07535)),
            Some((1.08152, 1.07535)),
            Some((1.08152, 1.07535)),
            Some((11.0199, 2.16653)),
            Some((11.0199, 2.16653)),
            Some((11.0199, 2.16653)),
            Some((11.0199, 2.16653)),
            Some((11.0199, 2.16653)),
            Some((11.0199, 2.16653)),
            Some((-48.4861, 56.0134)),
            Some((17.9093, -1.47755)),
            Some((13.3662, 6.08065)),
            Some((9.5805, 10.2259)),
            Some((-25.2259, 37.9469)),
            Some((-5.83286, 24.1871)),
            Some((12.8578, 7.79875)),
            Some((17.5803, 2.83235)),
            Some((11.2807, 11.6481)),
            Some((5.69381, 14.2098)),
            Some((5.69381, 14.2098)),
            Some((-10.0086, 27.6271)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
            Some((52.5931, -16.0985)),
        ],
        metpy_right_mover: (8.42187, 6.55198),
        metpy_mean_wind: (1.258, 4.33186),
    },
    Case {
        volume: "CBW (holdout)",
        layers: &[
            Some((1.75186, 2.8608)),
            Some((1.0478, 0.271181)),
            Some((-0.062117, 0.016039)),
            Some((-0.030252, 0.266885)),
            Some((-0.200995, -0.129083)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
            Some((0.006285, -0.273738)),
        ],
        metpy_right_mover: (-5.58563, 4.80122),
        metpy_mean_wind: (0.112218, -0.0757233),
    },
    Case {
        volume: "LSX (holdout)",
        layers: &[
            Some((1.92474, -2.52168)),
            Some((3.00523, -4.10241)),
            Some((3.38902, -5.09156)),
            Some((3.16775, -5.40574)),
            Some((3.75274, -5.19895)),
            Some((3.93018, -3.89193)),
            Some((4.21213, -1.86026)),
            Some((5.60192, 0.062199)),
            Some((6.80475, -4.55491)),
            Some((9.69358, -11.5806)),
            Some((7.71517, -8.78449)),
            Some((3.41785, -4.88705)),
            Some((-2.32319, -0.878745)),
            Some((-0.838594, -0.693363)),
            Some((1.54693, -0.812732)),
            Some((4.03428, -0.358106)),
            Some((6.53732, -1.14302)),
            Some((5.89951, -1.87323)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
            Some((-2.66224, 2.27014)),
        ],
        metpy_right_mover: (8.9794, 1.84247),
        metpy_mean_wind: (3.3909, -3.1594),
    },
    Case {
        volume: "MAF (holdout)",
        layers: &[
            Some((-1.34322, -1.7085)),
            Some((-2.27078, -3.47021)),
            Some((-2.62488, -4.92886)),
            Some((-0.012707, -3.16603)),
            Some((3.86233, -0.228924)),
            Some((1.59634, -0.333734)),
            Some((1.28467, -0.679422)),
            Some((1.6951, -2.22787)),
            Some((2.21082, -2.63963)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
            Some((1.32161, -1.08621)),
        ],
        metpy_right_mover: (4.79712, -8.06615),
        metpy_mean_wind: (0.903214, -1.6562),
    },
];

/// A quiet clear-air volume whose shear falls under rustdar's local floor,
/// where rustdar deliberately departs from the published formulation.
const CALM: Case = Case {
    volume: "HNX (decision)",
    layers: &[
        Some((-0.96657, -0.034286)),
        Some((-0.773151, 0.51421)),
        Some((-0.314282, 0.757724)),
        Some((0.027377, 0.413566)),
        Some((-0.007735, 0.16475)),
        Some((-0.238759, 0.350483)),
        Some((0.129892, 0.152793)),
        Some((0.021522, 0.200201)),
        Some((-0.131951, -0.092838)),
        Some((0.463716, 0.382699)),
        Some((0.000941, -0.080101)),
        Some((-0.207461, -0.040739)),
        Some((-0.232508, -0.043238)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
        Some((0.527902, -0.154919)),
    ],
    metpy_right_mover: (-2.80034, -6.82292),
    metpy_mean_wind: (0.0511171, 0.113884),
};

/// The formulation agrees with MetPy's to within the averaging convention.
///
/// The two differ only in how each averages a band: MetPy takes a
/// pressure-weighted trapezoidal mean of a continuous profile, this crate
/// takes a plain mean of its 0.3 km layer centres, which also skews each band
/// by half a layer. Measured across the corpus that is worth about a knot and
/// never more than ~2.
///
/// The bar is set just above that, and it is discriminating: widening the
/// mean-wind layer to 8.4 km, moving either shear band by one layer, changing
/// the deviation from 7.5 m/s, or returning the left mover all push at least
/// one of these volumes past it.
#[test]
fn bunkers_right_mover_matches_metpy_on_real_vad_profiles() {
    for case in SHEARED {
        let ours = bunkers_right_mover_uv(&profile(case.layers))
            .expect("a sheared profile supports Bunkers");
        let apart = knots_apart(ours, case.metpy_right_mover);
        assert!(
            apart < 2.5,
            "{}: rustdar {:?} vs MetPy {:?} — {apart:.2} kt apart, more than the \
             averaging convention accounts for",
            case.volume,
            ours,
            case.metpy_right_mover,
        );
    }
}

/// The deviation goes to the **right** of the shear. MetPy's left mover is
/// the mean wind's reflection of its right mover, and ours must sit nearer
/// the right one — a sign slip in `(S x k)` lands on the left mover exactly.
#[test]
fn the_deviation_is_right_of_the_shear_the_way_metpy_has_it() {
    for case in SHEARED {
        let ours = bunkers_right_mover_uv(&profile(case.layers)).expect("supports Bunkers");
        let metpy_left = (
            2.0 * case.metpy_mean_wind.0 - case.metpy_right_mover.0,
            2.0 * case.metpy_mean_wind.1 - case.metpy_right_mover.1,
        );
        assert!(
            knots_apart(ours, case.metpy_right_mover) < knots_apart(ours, metpy_left),
            "{}: rustdar {:?} sits nearer MetPy's LEFT mover {metpy_left:?} than its right \
             mover {:?} — the convention is flipped",
            case.volume,
            ours,
            case.metpy_right_mover,
        );
    }
}

/// Under its own shear floor rustdar returns the 0-6 km mean wind and drops
/// the deviation term entirely. That is a departure from the published
/// formulation rather than an arithmetic error, and this pins both halves of
/// that sentence against MetPy: the wind rustdar falls back to is the mean
/// wind MetPy computes, and MetPy's right mover is a further 7.5 m/s away —
/// the whole deviation, which rustdar is not applying.
///
/// If the floor is ever removed, this test is the one that should fail.
#[test]
fn under_the_shear_floor_rustdar_returns_metpys_mean_wind_and_omits_the_deviation() {
    let ours = bunkers_right_mover_uv(&profile(CALM.layers)).expect("falls back to the mean");
    assert!(
        knots_apart(ours, CALM.metpy_mean_wind) < 0.5,
        "the fallback should be the 0-6 km mean wind MetPy computes: rustdar {ours:?} vs \
         MetPy mean {:?}",
        CALM.metpy_mean_wind,
    );
    let deviation = ((CALM.metpy_right_mover.0 - CALM.metpy_mean_wind.0).powi(2)
        + (CALM.metpy_right_mover.1 - CALM.metpy_mean_wind.1).powi(2))
    .sqrt();
    assert!(
        (deviation - 7.5).abs() < 0.05,
        "MetPy still applies the full 7.5 m/s deviation here ({deviation:.3} m/s)",
    );
    assert!(
        knots_apart(ours, CALM.metpy_right_mover) > 10.0,
        "so rustdar's answer is far from MetPy's right mover: {ours:?} vs {:?}",
        CALM.metpy_right_mover,
    );
}
