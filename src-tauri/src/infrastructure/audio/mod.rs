//! Local voice output via Piper TTS (feature = "tts").
//!
//! The `Speaker` handle is always present so the actor code is unconditional;
//! when the `tts` feature is off, `speak` is a no-op. When on, a dedicated OS
//! thread streams each sentence through `piper | aplay`, so audio flows in
//! parallel with inference and the UI never blocks.

#[cfg(feature = "tts")]
use std::sync::mpsc;

/// Cheap, cloneable handle the actor uses to voice complete sentences.
#[derive(Clone, Default)]
pub struct Speaker {
    #[cfg(feature = "tts")]
    tx: Option<mpsc::Sender<String>>,
}

impl Speaker {
    /// A disabled speaker (no audio). Used when TTS is off or unconfigured.
    pub fn disabled() -> Self {
        Self::default()
    }

    /// Queue a full sentence for synthesis. Non-blocking; never stalls the UI.
    pub fn speak(&self, sentence: String) {
        #[cfg(feature = "tts")]
        {
            if let Some(tx) = &self.tx {
                let _ = tx.send(sentence);
            }
        }
        #[cfg(not(feature = "tts"))]
        {
            let _ = sentence;
        }
    }
}

/// Spawn the Piper playback worker and return a connected `Speaker`.
/// Requires a configured model; otherwise returns a disabled speaker.
#[cfg(feature = "tts")]
pub fn spawn(config: &crate::core::SystemConfig) -> Speaker {
    if config.piper_model.trim().is_empty() {
        tracing::warn!("KENSHO_PIPER_MODEL not set; voice disabled");
        return Speaker::disabled();
    }

    let (tx, rx) = mpsc::channel::<String>();
    let bin = config.piper_bin.clone();
    let model = config.piper_model.clone();
    let rate = config.piper_sample_rate;

    std::thread::spawn(move || {
        tracing::info!(%model, "Piper TTS worker started");
        // Sentences play sequentially, in order, as they arrive.
        while let Ok(sentence) = rx.recv() {
            play_sentence(&bin, &model, rate, &sentence);
        }
        tracing::info!("Piper TTS worker stopped");
    });

    Speaker { tx: Some(tx) }
}

/// Blocking: synthesize one sentence with Piper and play it via `aplay`.
#[cfg(feature = "tts")]
fn play_sentence(bin: &str, model: &str, rate: u32, sentence: &str) {
    use std::io::Write;
    use std::process::{Command, Stdio};

    // piper emits raw S16_LE mono PCM on stdout → aplay renders it.
    let pipeline = format!(
        "\"{bin}\" --model \"{model}\" --output-raw | aplay -q -r {rate} -f S16_LE -c 1 -t raw -"
    );

    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(&pipeline)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to spawn piper/aplay");
            return;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(sentence.as_bytes());
        // Drop stdin to signal EOF so piper finishes synthesis.
    }
    let _ = child.wait();
}
