use anyhow::{Context, Result};
use std::fs::File;
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub fn strip_urls_for_tts(input: &str) -> String {
    input
        .split_whitespace()
        .filter(|token| {
            let lower = token.to_ascii_lowercase();
            !(lower.starts_with("http://")
                || lower.starts_with("https://")
                || lower.starts_with("www."))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize_wav_header(audio: &[u8]) -> Vec<u8> {
    if audio.len() < 12 || &audio[0..4] != b"RIFF" || &audio[8..12] != b"WAVE" {
        return audio.to_vec();
    }

    let mut out = audio.to_vec();

    // Rewrite RIFF chunk size to actual file size - 8. Some TTS providers
    // return streaming-style sentinel values (0xFFFFFFFF), which strict WAV
    // parsers reject even though the payload itself is fine.
    let riff_size = (out.len().saturating_sub(8)).min(u32::MAX as usize) as u32;
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());

    // Walk chunks and fix the data chunk size if it uses a sentinel or would
    // otherwise overrun the file.
    let mut i = 12usize;
    while i + 8 <= out.len() {
        let chunk_id = [out[i], out[i + 1], out[i + 2], out[i + 3]];
        let chunk_size = u32::from_le_bytes([out[i + 4], out[i + 5], out[i + 6], out[i + 7]]) as usize;
        let data_start = i + 8;

        if chunk_id == *b"data" {
            let actual_size = out.len().saturating_sub(data_start).min(u32::MAX as usize) as u32;
            out[i + 4..i + 8].copy_from_slice(&actual_size.to_le_bytes());
            break;
        }

        let next = data_start.saturating_add(chunk_size).saturating_add(chunk_size % 2);
        if next <= i || next > out.len() {
            break;
        }
        i = next;
    }

    out
}

pub fn maybe_convert_ogg_opus_to_wav(input: &Path) -> Result<PathBuf> {
    let ext = input
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if !matches!(ext.as_str(), "oga" | "ogg" | "opus") {
        return Ok(input.to_path_buf());
    }

    let input_file =
        File::open(input).with_context(|| format!("opening audio file {}", input.display()))?;
    let (samples, header) = ogg_opus::decode::<_, 48000>(input_file)
        .with_context(|| format!("decoding ogg/opus file {}", input.display()))?;

    let output = input.with_extension("wav");
    let spec = hound::WavSpec {
        channels: header.channels,
        sample_rate: 48_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(&output, spec)
        .with_context(|| format!("creating wav file {}", output.display()))?;
    for sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    Ok(output)
}

/// Prepare synthesized TTS audio for sending. WAV responses are normalized and
/// converted to OGG/Opus; other supported formats pass through unchanged.
pub fn prepare_tts_audio_for_send(audio: Vec<u8>, format: &str) -> Result<(Vec<u8>, &'static str)> {
    if format != "wav" {
        // MP3 or other formats — pass through, use audio/mpeg
        let mime = match format {
            "mp3" => "audio/mpeg",
            "opus" | "ogg" => "audio/ogg",
            _ => "application/octet-stream",
        };
        return Ok((audio, mime));
    }

    let normalized = normalize_wav_header(&audio);
    let cursor = Cursor::new(&normalized);
    let reader = hound::WavReader::new(cursor).context("parsing WAV from TTS response")?;
    let spec = reader.spec();
    let samples: Vec<i16> = if spec.bits_per_sample == 16 && spec.sample_format == hound::SampleFormat::Int {
        reader.into_samples::<i16>().collect::<std::result::Result<Vec<_>, _>>()
            .context("reading WAV samples")?
    } else {
        // Convert from other formats (e.g. 32-bit float) to i16
        let reader2 = hound::WavReader::new(Cursor::new(&normalized))
            .context("re-parsing normalized WAV from TTS response")?;
        reader2.into_samples::<f32>()
            .map(|s| s.map(|v| (v * 32767.0).clamp(-32768.0, 32767.0) as i16))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("converting WAV samples to i16")?
    };

    let ogg = match (spec.sample_rate, spec.channels) {
        (48000, 1) => ogg_opus::encode::<48000, 1>(&samples),
        (48000, 2) => ogg_opus::encode::<48000, 2>(&samples),
        (24000, 1) => ogg_opus::encode::<24000, 1>(&samples),
        (24000, 2) => ogg_opus::encode::<24000, 2>(&samples),
        (16000, 1) => ogg_opus::encode::<16000, 1>(&samples),
        (16000, 2) => ogg_opus::encode::<16000, 2>(&samples),
        (sr, ch) => return Err(anyhow::anyhow!("unsupported WAV format: {sr}Hz {ch}ch")),
    }
    .context("encoding OGG/Opus")?;

    Ok((ogg, "audio/ogg"))
}
