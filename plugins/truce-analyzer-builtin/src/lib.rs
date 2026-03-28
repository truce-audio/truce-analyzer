use truce::prelude::*;

#[derive(Params)]
pub struct TruceAnalyzerBuiltinParams {
    #[param(name = "Gain", range = "linear(-60, 6)",
            unit = "dB", smooth = "exp(5)")]
    pub gain: FloatParam,
}

use TruceAnalyzerBuiltinParamsParamId as P;

pub struct TruceAnalyzerBuiltin {
    params: Arc<TruceAnalyzerBuiltinParams>,
}

impl TruceAnalyzerBuiltin {
    pub fn new(params: Arc<TruceAnalyzerBuiltinParams>) -> Self {
        Self { params }
    }
}

impl PluginLogic for TruceAnalyzerBuiltin {
    fn reset(&mut self, sr: f64, _bs: usize) {
        self.params.set_sample_rate(sr);
    }

    fn process(&mut self, buffer: &mut AudioBuffer, _events: &EventList,
               _context: &mut ProcessContext) -> ProcessStatus {
        for i in 0..buffer.num_samples() {
            let gain = db_to_linear(self.params.gain.smoothed_next() as f64) as f32;
            for ch in 0..buffer.channels() {
                let (inp, out) = buffer.io(ch);
                out[i] = inp[i] * gain;
            }
        }
        ProcessStatus::Normal
    }

    fn layout(&self) -> truce_gui::layout::GridLayout {
        use truce_gui::layout::{GridLayout, GridWidget};
        GridLayout::build("TruceAnalyzerBuiltin", "V0.1", 2, 80.0, vec![
            GridWidget::knob(P::Gain, "Gain"),
        ], vec![])
    }
}

truce::plugin! {
    logic: TruceAnalyzerBuiltin,
    params: TruceAnalyzerBuiltinParams,
    bus_layouts: [BusLayout::stereo()],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_runs() {
        let result = truce_test::render_effect::<Plugin>(512, 44100.0);
        truce_test::assert_no_nans(&result.output);
    }
}
