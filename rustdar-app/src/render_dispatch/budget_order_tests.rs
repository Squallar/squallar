#[test]
fn the_budget_is_read_before_the_volume_is_walked() {
    let src = include_str!("../render_dispatch.rs");
    let (_, body) = src
        .split_once("pub fn spawn_level2_render(")
        .expect("spawn_level2_render is no longer a method here");
    let body = body
        .split_once("\n    }")
        .map(|(body, _)| body)
        .expect("spawn_level2_render has no recognisable body");

    let gate = body
        .find("render_slot_free()")
        .expect("spawn_level2_render no longer asks whether a slot is free");
    let walk = body
        .find("RenderInput::extract(")
        .expect("spawn_level2_render no longer extracts a payload");
    assert!(
        gate < walk,
        "the volume is walked before the budget is read, so a refused dispatch \
         pays the whole extraction and discards it — every frame, for as long \
         as the budget is full",
    );
}
