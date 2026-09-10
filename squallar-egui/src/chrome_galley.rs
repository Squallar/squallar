//! The chrome's button labels, laid out once and handed back until something
//! they read moves.
//!
//! **A button's galley is a pure function of its text and the `Style`.** That
//! is not an assumption about our themes, it is what `egui::Button::atom_ui`
//! computes: the fallback font it hands the layout is
//! `Style::widget_style`'s, and that reads `Style::override_font_id` or
//! `TextStyle::Body` and nothing else — `WidgetState` and the style classes
//! reach the *colour* and never the font. The colour a plain label lays out
//! with is `Color32::PLACEHOLDER`, replaced at paint time by whatever the
//! button's state picked. So hover, focus, press and disable move a button's
//! paint and leave its galley alone, and a table keyed without them is exact
//! rather than approximate.
//!
//! What that layout costs when nothing has changed is the point. `epaint`'s
//! own galley cache does hit — `text_layout::layout` runs 0.84 times a frame
//! on this fixture, against 70 `layout_job` calls — so the text is not being
//! re-shaped. It is the asking that costs: a `LayoutJob` built from a `&str`
//! the caller already holds, an `ahash` over the whole job, and a
//! `Context::fonts_mut` write lock on the entire egui context to be handed
//! back last frame's `Arc`. Measured on the desktop fixture below, that is
//! 915 Ir per button and 39,330 Ir a frame across the 43 buttons a frame
//! draws.
//!
//! # Only where the wrap mode is `Extend`
//!
//! [`Spec::widget_text`] declines outside it, and the caller then builds the
//! text it always built. Under `Wrap` or `Truncate` the galley depends on the
//! width the atom is given, and that width is
//! `available_size - frame.total_margin()` where the frame margin carries
//! `WidgetVisuals::expansion` — which *is* state-dependent. A memo that
//! guessed it would hand a hovered button the inactive button's wrapping. The
//! decline is the whole of the miss path: one `Ui::wrap_mode` read, and then
//! the same `RichText` the call site wrote before.
//!
//! # What is stamped, and why each term
//!
//! `pixels_per_point` re-rasterizes every glyph. The **atlas generation** is
//! [`walkers::GalleyCache::generation`], the number `Gui::ui_phased` already
//! maintains at the head of the pass: a `Galley` addresses the font atlas in
//! pixels, and egui rebuilds that atlas under the pass when it passes 80 %
//! full and puts every glyph somewhere else. A galley kept across that draws
//! the right words in the wrong letters, which is `8f2e1bb0f`. Both are
//! table-wide: when either moves nothing kept is worth anything.
//!
//! The `Style` is in the key as [`Resolved`] — the `FontId` and `Color32` the
//! layout will actually be handed — and **not** as the identity of the
//! `Arc<Style>` the `Ui` holds. That was the first shape of this and it was
//! wrong in a way that reads as working: `Ui::style_mut` is copy-on-write, so
//! a scope that sets its own `item_spacing` gets a fresh `Arc` every frame,
//! and every call site under it missed for ever while paying the probe. Two
//! such lines — the top bar's `interact_size` and the status bar's
//! `item_spacing` — were 10 of 29 sites, measured. Spacing is not something a
//! galley reads; resolving the two terms that are costs a `BTreeMap` lookup
//! and answers for every `Style` that would lay the same text out the same
//! way.
//!
//! # Keyed by the widget's own id
//!
//! One entry per call site, not one per string: `Ui::next_auto_id` is the id
//! the button is about to register under, so the table is bounded by the
//! chrome's widget count and cannot grow with the session. The stored text is
//! compared on every hit, so a call site whose label changed — the timeline's
//! clock, a row that was renamed — misses and re-lays. The id is not trusted
//! to imply the text; it is only what makes the probe a `u64`.

use std::sync::Arc;

/// What a chrome button's label is, in the terms `egui::RichText` resolves.
///
/// Deliberately a description rather than a built `RichText`: building one
/// allocates a `String` from a `&str` the caller already holds, and the hit
/// path exists to make that allocation not happen.
#[derive(Clone, Copy)]
pub(crate) struct Spec<'a> {
    text: &'a str,
    size: Option<f32>,
    color: Option<egui::Color32>,
    weak: bool,
}

impl<'a> Spec<'a> {
    /// A plain label — what `ui.button("Add layer")` lays out.
    pub(crate) fn plain(text: &'a str) -> Self {
        Self {
            text,
            size: None,
            color: None,
            weak: false,
        }
    }

    /// `RichText::new(text).color(color)`.
    pub(crate) fn colored(text: &'a str, color: egui::Color32) -> Self {
        Self {
            color: Some(color),
            ..Self::plain(text)
        }
    }

    /// `RichText::new(text).size(size)`.
    pub(crate) fn sized(text: &'a str, size: f32) -> Self {
        Self {
            size: Some(size),
            ..Self::plain(text)
        }
    }

    /// `RichText::new(text).size(size).color(color)`.
    pub(crate) fn sized_color(text: &'a str, size: f32, color: egui::Color32) -> Self {
        Self {
            color: Some(color),
            ..Self::sized(text, size)
        }
    }

    /// `RichText::new(text).size(size).weak()`.
    pub(crate) fn sized_weak(text: &'a str, size: f32) -> Self {
        Self {
            weak: true,
            ..Self::sized(text, size)
        }
    }

    /// The text egui would have been handed, built the way the call site
    /// built it before this memo existed.
    ///
    /// This is the miss path *and* the definition of the hit path: a stored
    /// galley is whatever `into_galley_impl` made of exactly this value.
    fn widget_text(self) -> egui::WidgetText {
        if self.size.is_none() && self.color.is_none() && !self.weak {
            return egui::WidgetText::Text(self.text.to_owned());
        }
        let mut rich = egui::RichText::new(self.text);
        if let Some(size) = self.size {
            rich = rich.size(size);
        }
        if let Some(color) = self.color {
            rich = rich.color(color);
        }
        if self.weak {
            rich = rich.weak();
        }
        egui::WidgetText::RichText(Arc::new(rich))
    }
}

/// Everything the layout reads off the `Style`, resolved.
///
/// **The whole of it.** `into_galley_impl` hands the font set a `TextFormat`
/// whose style-derived terms are exactly a `FontId` and a `Color32`; every
/// other field of that format — italics, underline, background, line height,
/// letter spacing — is a [`Spec`] term this type does not offer and so keeps
/// at `RichText`'s default. `valign` is keyed beside this, and the wrap is
/// `Extend` or the call declined.
#[derive(Clone, PartialEq)]
struct Resolved {
    /// Compared by bits rather than by `f32` equality, which is stricter and
    /// therefore the safe direction: a spurious miss costs one layout, a
    /// spurious hit puts the wrong glyphs on the glass.
    size: u32,
    family: egui::FontFamily,
    color: egui::Color32,
}

/// Resolve `spec` against `style` the way `into_galley_impl` will.
///
/// The font is `Style::override_font_id`, else `Style::override_text_style`,
/// else `TextStyle::Body` — which is the fallback `Button::atom_ui` passes,
/// because `Style::widget_style` reads those three and nothing else. The
/// widget's `WidgetState` and its style classes reach the *colour* of a
/// button and never its font, so nothing here needs last frame's response.
///
/// The colour is `RichText::get_text_color`'s ladder, and `PLACEHOLDER` where
/// that yields nothing — the sentinel the button replaces at paint time with
/// whatever its state picked. That is what keeps a hover out of this key.
fn resolve(style: &egui::Style, spec: Spec<'_>) -> Resolved {
    let font = style.override_font_id.clone().unwrap_or_else(|| {
        style.override_text_style.as_ref().map_or_else(
            || egui::TextStyle::Body.resolve(style),
            |ts| ts.resolve(style),
        )
    });
    let size = spec.size.unwrap_or(font.size);
    let color = if let Some(color) = spec.color {
        color
    } else if spec.weak {
        style.visuals.weak_text_color()
    } else {
        style
            .visuals
            .override_text_color
            .unwrap_or(egui::Color32::PLACEHOLDER)
    };
    Resolved {
        size: size.to_bits(),
        family: font.family,
        color,
    }
}

/// One call site's laid-out label.
struct Kept {
    /// A `String` rather than a `Box<str>` so that a call site whose label
    /// moves — the timeline's clock, a row that gained a status — refills the
    /// buffer it already has instead of handing one back to the allocator and
    /// asking for another on every frame it changes.
    text: String,
    resolved: Resolved,
    valign: egui::Align,
    galley: Arc<egui::Galley>,
}

/// The chrome's laid-out button labels, kept between frames.
///
/// Owned by [`crate::gui::Gui`] for the reason every other memo here is: what
/// it saves only exists across frames, and one owner with a visible lifetime
/// is what lets a test hold its own. Empty is always correct — every entry is
/// reproducible from the [`Spec`] that made it.
#[derive(Default)]
pub(crate) struct ChromeGalleys {
    entries: egui::IdMap<Kept>,
    pixels_per_point: u32,
    generation: u64,
    layouts: u64,
    hits: u64,
    /// Answer nothing, so every call site builds the text it built before
    /// this table existed. The other half of the glass pin below: a memo can
    /// only be shown to change nothing by running the frame both ways.
    #[cfg(test)]
    bypass: bool,
}

impl ChromeGalleys {
    /// The most call sites this table holds before it drops the lot.
    ///
    /// **Bounded by the chrome, not by the session — but only by argument,
    /// and an argument is not a ceiling.** One entry per `Ui::next_auto_id`,
    /// and the chrome's own buttons are a fixed set; what is not fixed is
    /// that a layer-stack row salts its widgets on the layer's own id, so a
    /// user who curates a pane across a long session retires ids this table
    /// would otherwise keep for ever. Dropping is
    /// [`walkers::GalleyCache::MAX_ENTRIES`]' trade and it costs the same: only
    /// the labels a frame actually draws are looked up, so the frame after a
    /// drop lays out that frame's labels and no others, and the table is back
    /// to the working set on that same frame. That is a degradation, never a
    /// cliff — it is not worse than having no table at all — where a ceiling
    /// that stopped *inserting* would avoid the frame and pay for it for ever.
    const MAX_ENTRIES: usize = 256;

    /// Drop everything if the glyph raster moved under us.
    ///
    /// `generation` is [`walkers::GalleyCache::generation`], read once a frame
    /// at the head of the pass; `ctx` supplies `pixels_per_point`. Called from
    /// `Gui::ui_phased` beside the read that maintains the generation, so this
    /// costs a `u64` compare and no lock of its own.
    pub(crate) fn begin_frame(&mut self, ctx: &egui::Context, generation: u64) {
        let pixels_per_point = ctx.pixels_per_point().to_bits();
        if self.generation != generation || self.pixels_per_point != pixels_per_point {
            self.entries.clear();
            self.generation = generation;
            self.pixels_per_point = pixels_per_point;
        }
    }

    /// The text to hand `egui::Button::new`.
    ///
    /// A hit is `WidgetText::Galley`, which `into_galley_impl` returns
    /// untouched. A miss — a new call site, a changed label, a restyled
    /// scope, or any wrap mode but `Extend` — is the `WidgetText` the call
    /// site would have built anyway.
    pub(crate) fn label(&mut self, ui: &egui::Ui, spec: Spec<'_>) -> egui::WidgetText {
        if ui.wrap_mode() != egui::TextWrapMode::Extend {
            return spec.widget_text();
        }
        #[cfg(test)]
        if self.bypass {
            return spec.widget_text();
        }
        let id = ui.next_auto_id();
        let valign = ui.text_valign();
        let style = ui.style();
        let resolved = resolve(style, spec);
        if let Some(kept) = self.entries.get(&id)
            && kept.valign == valign
            && kept.resolved == resolved
            && *kept.text == *spec.text
        {
            self.hits += 1;
            return egui::WidgetText::Galley(kept.galley.clone());
        }
        let galley = lay_out(ui, spec);
        self.layouts += 1;
        match self.entries.entry(id) {
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                let kept = slot.get_mut();
                kept.text.clear();
                kept.text.push_str(spec.text);
                kept.resolved = resolved;
                kept.valign = valign;
                kept.galley = galley.clone();
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(Kept {
                    text: spec.text.to_owned(),
                    resolved,
                    valign,
                    galley: galley.clone(),
                });
            }
        }
        if self.entries.len() > Self::MAX_ENTRIES {
            self.entries.clear();
        }
        egui::WidgetText::Galley(galley)
    }

    /// Stop answering, so the frame lays every label out the way it did
    /// before this table existed. See the glass pin.
    #[cfg(test)]
    pub(crate) fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
        self.entries.clear();
    }

    /// Labels laid out — one per call site and description this table has
    /// seen.
    #[cfg(test)]
    pub(crate) fn layouts(&self) -> u64 {
        self.layouts
    }

    /// Labels answered from a kept galley.
    #[cfg(test)]
    pub(crate) fn hits(&self) -> u64 {
        self.hits
    }

    /// Entries held — one per call site the table has answered.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Lay `spec` out exactly as `Button::atom_ui` would have.
///
/// The fallback font is spelled the way `Style::widget_style` spells it,
/// because that is the value `atom_ui` passes: `override_font_id` if the
/// style names one, `TextStyle::Body` otherwise. Nothing here reads the
/// widget's state, because that path does not either.
fn lay_out(ui: &egui::Ui, spec: Spec<'_>) -> Arc<egui::Galley> {
    let style = ui.style();
    let fallback = egui::FontSelection::FontId(
        style
            .override_font_id
            .clone()
            .unwrap_or_else(|| egui::TextStyle::Body.resolve(style)),
    );
    let wrapping = egui::epaint::text::TextWrapping::from_wrap_mode_and_width(
        egui::TextWrapMode::Extend,
        f32::INFINITY,
    );
    spec.widget_text()
        .into_galley_impl(ui.ctx(), style, wrapping, fallback, ui.text_valign())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_harness::InputHarness;

    /// Run one egui pass and hand the body a horizontal `Ui`, so the wrap
    /// mode is `Extend` — the only one the memo answers in — and
    /// `Ui::next_auto_id` is the same id on every pass.
    fn pass<R>(ctx: &egui::Context, mut body: impl FnMut(&mut egui::Ui) -> R) -> R {
        let mut out = None;
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                out = Some(body(ui));
            });
        });
        out.expect("the body runs once per pass")
    }

    /// A pass that lays nothing out, so `epaint`'s own galley cache — which
    /// keeps only what the last pass used — drops everything this table is
    /// holding a reference to. That is what makes [`Arc::ptr_eq`] below
    /// decisive rather than accidental: after it, an equal galley from a
    /// fresh layout is a *different* allocation, so a pointer match can only
    /// mean the memo answered.
    fn empty_pass(ctx: &egui::Context) {
        for _ in 0..2 {
            let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        }
    }

    /// What a settled chrome asks for, and what it should not ask twice.
    ///
    /// The fixture is the desktop one the memo was measured on: 1024x768,
    /// `Expanded`, persistent sidebar, the layers panel open on seven real
    /// rows. It carries the property the win needs to be visible — real
    /// buttons with real labels, on frames where nothing changed.
    #[test]
    fn a_settled_frame_lays_no_button_label_out_and_answers_every_one_from_the_table() {
        let mut h = InputHarness::new();
        h.warm_up();
        h.frame();

        let table = h.gui().chrome_galleys_for_test();
        let entries = table.len();
        assert!(
            entries >= 20,
            "{entries} memoized call sites is too few for this pin to mean \
             anything: it exists because the chrome draws dozens of buttons a \
             frame, and a fixture that draws a handful cannot show one being \
             asked twice",
        );

        let before = (
            h.gui().chrome_galleys_for_test().hits(),
            h.gui().chrome_galleys_for_test().layouts(),
        );
        for _ in 0..5 {
            h.frame();
        }
        let after = (
            h.gui().chrome_galleys_for_test().hits(),
            h.gui().chrome_galleys_for_test().layouts(),
        );

        assert_eq!(
            after.1 - before.1,
            0,
            "five frames of a chrome nobody touched laid {} button label(s) \
             out again. Each one is a `LayoutJob` built from a `&str` the call \
             site already holds, an `ahash` over that whole job, and a \
             `Context::fonts_mut` write lock on the entire egui context, to be \
             handed back the `Arc` it was handed last frame.",
            after.1 - before.1,
        );
        assert_eq!(
            after.0 - before.0,
            (entries * 5) as u64,
            "five frames answered {} of the {} labels a frame from the table",
            after.0 - before.0,
            entries * 5,
        );
    }

    /// A kept galley is the one egui would have laid out, not merely one that
    /// looks like it.
    #[test]
    fn a_kept_label_is_the_galley_a_fresh_layout_produces() {
        let ctx = egui::Context::default();
        let mut table = ChromeGalleys::default();

        let first = pass(&ctx, |ui| {
            let egui::WidgetText::Galley(galley) = table.label(ui, Spec::plain("Add layer")) else {
                panic!("the memo declined a plain label in a panel");
            };
            galley
        });
        let fresh = pass(&ctx, |ui| lay_out(ui, Spec::plain("Add layer")));
        assert_eq!(
            first.rows.len(),
            fresh.rows.len(),
            "the memo laid a different shape out than egui's own path"
        );
        assert_eq!(first.size(), fresh.size(), "size");
        assert_eq!(first.text(), fresh.text(), "text");

        // Nothing lays out for two passes, so epaint's galley cache retires
        // everything and only the table can still be holding this `Arc`.
        empty_pass(&ctx);
        let again = pass(&ctx, |ui| {
            let egui::WidgetText::Galley(galley) = table.label(ui, Spec::plain("Add layer")) else {
                panic!("the memo declined a plain label in a panel");
            };
            galley
        });
        assert!(
            Arc::ptr_eq(&first, &again),
            "the label was laid out again across a pass that evicted every \
             other reference to it, so nothing was kept"
        );
    }

    /// Every term the layout reads is in the key, one arm each. A term that
    /// is not keyed is a hit that puts last frame's glyphs on the glass.
    #[test]
    fn every_term_the_layout_reads_is_keyed() {
        /// One arm: a name, and the mutation that should make the second
        /// pass miss.
        type Arm = (&'static str, Box<dyn Fn(&mut egui::Ui) -> Spec<'static>>);

        let cases: Vec<Arm> = vec![
            ("text", Box::new(|_: &mut egui::Ui| Spec::plain("Remove"))),
            (
                "size",
                Box::new(|_: &mut egui::Ui| Spec::sized("Add layer", 19.0)),
            ),
            (
                "colour",
                Box::new(|_: &mut egui::Ui| Spec::colored("Add layer", egui::Color32::RED)),
            ),
            (
                "weak",
                Box::new(|_: &mut egui::Ui| Spec::sized_weak("Add layer", 14.0)),
            ),
            (
                "font",
                Box::new(|ui: &mut egui::Ui| {
                    ui.style_mut().override_font_id = Some(egui::FontId::monospace(21.0));
                    Spec::plain("Add layer")
                }),
            ),
            (
                "valign",
                Box::new(|ui: &mut egui::Ui| {
                    ui.style_mut().override_text_valign = Some(egui::Align::TOP);
                    Spec::plain("Add layer")
                }),
            ),
        ];

        for (term, mutate) in cases {
            let ctx = egui::Context::default();
            let mut table = ChromeGalleys::default();
            pass(&ctx, |ui| {
                table.label(ui, Spec::plain("Add layer"));
            });
            let hits = table.hits();
            let layouts = table.layouts();
            pass(&ctx, |ui| {
                let spec = mutate(ui);
                table.label(ui, spec);
            });
            assert_eq!(
                table.hits(),
                hits,
                "a label whose {term} moved was answered from the table, so \
                 that term is not in the key and the wrong glyphs reach the \
                 glass",
            );
            assert_eq!(table.layouts(), layouts + 1, "{term} should re-lay");
        }

        // The control: the same call site, unmutated, does hit — otherwise
        // every arm above would pass with the memo bolted shut.
        let ctx = egui::Context::default();
        let mut table = ChromeGalleys::default();
        pass(&ctx, |ui| {
            table.label(ui, Spec::plain("Add layer"));
        });
        pass(&ctx, |ui| {
            table.label(ui, Spec::plain("Add layer"));
        });
        assert_eq!(
            table.hits(),
            1,
            "the unchanged control did not hit, so the arms above prove nothing"
        );
    }

    /// A galley addresses the font atlas in pixels, and egui rebuilds that
    /// atlas under the pass. Everything kept here is worthless the moment it
    /// does.
    #[test]
    fn a_moved_atlas_generation_drops_every_kept_label() {
        let ctx = egui::Context::default();
        let mut table = ChromeGalleys::default();
        // Stamped before anything lays out, which is where `Gui::ui_phased`
        // calls it and the only order in which the comparison below means
        // anything.
        table.begin_frame(&ctx, 0);
        pass(&ctx, |ui| {
            table.label(ui, Spec::plain("Add layer"));
        });
        assert_eq!(table.len(), 1);

        table.begin_frame(&ctx, 0);
        assert_eq!(table.len(), 1, "an unmoved raster keeps the table");

        table.begin_frame(&ctx, 1);
        assert_eq!(
            table.len(),
            0,
            "the glyph raster moved and the table kept galleys addressing \
             texels that now hold other letters"
        );
    }

    /// The table has a ceiling, and it drops rather than stops inserting.
    ///
    /// The ids a stack row salts on the layer it draws are the ones that
    /// retire; nothing else here grows.
    #[test]
    fn the_table_is_dropped_once_it_outgrows_its_ceiling() {
        let ctx = egui::Context::default();
        let mut table = ChromeGalleys::default();
        pass(&ctx, |ui| {
            for slot in 0..=ChromeGalleys::MAX_ENTRIES {
                ui.push_id(slot, |ui| {
                    table.label(ui, Spec::plain("Add layer"));
                });
            }
        });
        assert_eq!(
            table.len(),
            0,
            "a table that had grown past its ceiling kept {} entries, so a \
             session that curates a pane for long enough holds every label it \
             ever drew",
            table.len(),
        );
    }

    /// Every painted rect, fill and text run of the whole frame, as one
    /// comparable value.
    fn glass(h: &InputHarness) -> Vec<String> {
        let mut out = Vec::new();
        for rect in h.painted_rects() {
            out.push(format!("R {rect:?}"));
        }
        for fill in h.painted_fills() {
            out.push(format!("F {fill:?}"));
        }
        for (rect, text) in h.painted_text_rects() {
            out.push(format!("T {rect:?} {text}"));
        }
        out
    }

    fn settled_glass(bypass: bool) -> Vec<String> {
        let mut h = InputHarness::new();
        h.gui_mut().set_chrome_galley_bypass_for_test(bypass);
        h.warm_up();
        h.frame();
        glass(&h)
    }

    /// **The memo changes nothing on the glass.**
    ///
    /// The same-arm control runs first and runs again last, so a difference
    /// that came from anywhere but the memo — a clock in a label, a fade that
    /// had not settled — is named as such instead of being read as the memo's
    /// doing.
    #[test]
    fn a_memoized_frame_paints_exactly_what_a_freshly_laid_out_one_paints() {
        let control_before = settled_glass(true);
        let memoized = settled_glass(false);
        let control_after = settled_glass(true);

        assert!(
            control_before.len() > 100,
            "{} painted things is too thin a frame for this pin to mean \
             anything",
            control_before.len(),
        );
        assert_eq!(
            control_before, control_after,
            "two runs of the SAME arm disagree, so the comparison below is \
             against harness noise and not against the memo"
        );
        assert_eq!(
            control_before, memoized,
            "the memoized frame painted something the freshly laid out one \
             did not. A kept galley that is not the one egui would have \
             produced looks right and is wrong: same rects, same colours, \
             different letters."
        );
    }

    /// Outside `Extend` the galley depends on the width the atom is given,
    /// and that width carries the widget's own state through
    /// `WidgetVisuals::expansion`. The memo has to decline rather than guess.
    #[test]
    fn a_wrapping_layout_is_declined_rather_than_guessed() {
        let ctx = egui::Context::default();
        let mut table = ChromeGalleys::default();
        let declined = pass(&ctx, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            matches!(
                table.label(ui, Spec::plain("Add layer")),
                egui::WidgetText::Text(_)
            )
        });
        assert!(
            declined,
            "a truncating layout was answered with a galley laid out at \
             infinite width"
        );
        assert_eq!(table.len(), 0, "a declined call must keep nothing");
    }
}
