//! Complete Multi-Bus Mixing Matrix (`Game`, `Chat`, `Media`, `Aux`, `Mic` -> `Master` & `VirtualMic`)
//! including SteelSeries Sonar-inspired ChatMix balance control.

use crate::denormals::ScopedNoDenormals;
use crate::smoother::SmoothedParam;
use crate::strip::ChannelStripProcessor;
use crate::voice::VoiceChainProcessor;

/// Compute the `[game_gain, chat_gain]` multiplier pair for a ChatMix balance
/// dial in `[-1.0, 1.0]`:
/// - `0.0`: Both Game and Chat are at `1.0` (0 dB).
/// - `< 0.0` (towards Game): Game stays `1.0`, Chat linearly tapers to `0.0` at `-1.0`.
/// - `> 0.0` (towards Chat): Chat stays `1.0`, Game linearly tapers to `0.0` at `+1.0`.
#[inline]
pub fn compute_chatmix_gains(balance: f32) -> (f32, f32) {
    let b = balance.clamp(-1.0, 1.0);
    let game_gain = if b <= 0.0 { 1.0 } else { 1.0 - b };
    let chat_gain = if b >= 0.0 { 1.0 } else { 1.0 + b };
    (game_gain, chat_gain)
}

/// Real-time Mixing Matrix owning all virtual sink channel strips, ChatMix smoothers,
/// and the Microphone Voice DSP chain.
pub struct AudioMixerMatrix {
    pub game_strip: ChannelStripProcessor,
    pub chat_strip: ChannelStripProcessor,
    pub media_strip: ChannelStripProcessor,
    pub aux_strip: ChannelStripProcessor,
    pub master_strip: ChannelStripProcessor,
    pub voice_chain: VoiceChainProcessor,

    chatmix_game_gain: SmoothedParam,
    chatmix_chat_gain: SmoothedParam,
}

impl AudioMixerMatrix {
    pub fn new(
        sample_rate: f32,
        game_strip: ChannelStripProcessor,
        chat_strip: ChannelStripProcessor,
        media_strip: ChannelStripProcessor,
        aux_strip: ChannelStripProcessor,
        master_strip: ChannelStripProcessor,
        voice_chain: VoiceChainProcessor,
    ) -> Self {
        Self {
            game_strip,
            chat_strip,
            media_strip,
            aux_strip,
            master_strip,
            voice_chain,
            chatmix_game_gain: SmoothedParam::new(1.0, 10.0, sample_rate),
            chatmix_chat_gain: SmoothedParam::new(1.0, 10.0, sample_rate),
        }
    }

    /// Updates the ChatMix balance dial in `[-1.0, 1.0]`.
    #[inline]
    pub fn set_chatmix_balance(&mut self, balance: f32) {
        let (g, c) = compute_chatmix_gains(balance);
        self.chatmix_game_gain.set_target(g);
        self.chatmix_chat_gain.set_target(c);
    }

    /// Executes one full real-time audio quantum across all virtual buses, sums them
    /// into `master_out_interleaved`, processes the `Master` EQ/fader strip, and mixes
    /// zero-feedback microphone sidetone onto the physical output.
    #[allow(clippy::too_many_arguments)]
    pub fn process_quantum(
        &mut self,
        game_buf: &mut [f32],
        chat_buf: &mut [f32],
        media_buf: &mut [f32],
        aux_buf: &mut [f32],
        mic_mono_in: &[f32],
        virtual_mic_out: &mut [f32],
        master_out: &mut [f32],
    ) {
        let _no_denormals = ScopedNoDenormals::new();

        // 1. Process individual virtual sink channel strips (EQ + Fader + Pan + Metering)
        self.game_strip.process_interleaved(game_buf);
        self.chat_strip.process_interleaved(chat_buf);
        self.media_strip.process_interleaved(media_buf);
        self.aux_strip.process_interleaved(aux_buf);

        // 2. Sum virtual sinks into Master bus with ChatMix weighting
        let len = master_out
            .len()
            .min(game_buf.len())
            .min(chat_buf.len())
            .min(media_buf.len())
            .min(aux_buf.len());

        for i in (0..len).step_by(2) {
            let cm_g = self.chatmix_game_gain.next_sample();
            let cm_c = self.chatmix_chat_gain.next_sample();

            master_out[i] = game_buf[i] * cm_g + chat_buf[i] * cm_c + media_buf[i] + aux_buf[i];
            master_out[i + 1] =
                game_buf[i + 1] * cm_g + chat_buf[i + 1] * cm_c + media_buf[i + 1] + aux_buf[i + 1];
        }

        // 3. Process Master channel strip (Master 10-band EQ + Master Fader + Master Metering)
        self.master_strip
            .process_interleaved(&mut master_out[..len]);

        // 4. Process Microphone Voice Chain & inject post-master zero-feedback Sidetone
        self.voice_chain
            .process_block(mic_mono_in, virtual_mic_out, &mut master_out[..len]);

        // 5. Hard safety limiter / soft-clipper at +/- 1.0 to protect ears & DAC
        for sample in &mut master_out[..len] {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
}
