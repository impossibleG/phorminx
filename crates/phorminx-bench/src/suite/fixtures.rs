//! Synthetic inputs: speech from the Windows speech synthesizer and
//! screenshots drawn with System.Drawing. The texts below are the references
//! that error rates are measured against; nothing here is a real recording.
//! Spoken texts avoid numbers: recognizers write "3" or "three" by
//! preference, and the error rate should measure hearing, not number style.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, ensure};
use phorminx_audio::{read_wav, write_wav};
use phorminx_core::AudioClip;
use serde_json::json;

use super::Assets;

pub struct Speech {
    pub id: &'static str,
    pub language: &'static str,
    /// Windows voice per line; lines alternate between them.
    pub voices: &'static [&'static str],
    pub lines: &'static [&'static str],
}

impl Speech {
    pub fn reference(&self) -> String {
        self.lines.join(" ")
    }
    pub fn wav(&self, assets: &Assets) -> PathBuf {
        assets.fixtures().join(format!("{}.wav", self.id))
    }
}

/// Seconds of silence before the first word of every recording.
pub const LEADING_SILENCE: f64 = 0.5;

pub const DICTATION: &[Speech] = &[
    Speech {
        id: "dictation-en",
        language: "en",
        voices: &["Microsoft Zira Desktop"],
        lines: &[
            "Please schedule the quarterly review for next Tuesday afternoon.",
            "We need to cover the budget, the hiring plan and the launch date.",
            "After that, send a short summary to the whole team and remind everyone to bring their notes.",
        ],
    },
    Speech {
        id: "dictation-pt",
        language: "pt",
        voices: &["Microsoft Maria Desktop"],
        lines: &[
            "Por favor, marque a reunião trimestral para a próxima terça-feira à tarde.",
            "Precisamos falar sobre o orçamento, o plano de contratação e a data de lançamento.",
            "Depois disso, mande um resumo curto para toda a equipe e lembre todos de trazer suas anotações.",
        ],
    },
];

pub const MEETINGS: &[Speech] = &[
    Speech {
        id: "meeting-en",
        language: "en",
        voices: &["Microsoft David Desktop", "Microsoft Zira Desktop"],
        lines: &[
            "Good morning everyone, let's start with the launch status.",
            "The mobile build passed review yesterday, so we can ship on Monday.",
            "Great. What is still blocking the web version?",
            "Two things. The payment page needs a final security check, and the help articles are not translated yet.",
            "Who can own the security check this week?",
            "I can do it by Thursday if someone covers support on Wednesday.",
            "I will cover support on Wednesday. For the translations, we already have a fixed budget.",
            "Then let's hire the agency we used last year and ask for delivery by the end of the month.",
            "Agreed. One more thing: the marketing email goes out the morning of the launch, not the night before.",
            "Noted. So the actions are the security check by Thursday, the agency by the end of the month, and the email on launch morning.",
            "Perfect. Let's meet again on Friday to confirm everything.",
        ],
    },
    Speech {
        id: "meeting-pt",
        language: "pt",
        voices: &["Microsoft Maria Desktop"],
        lines: &[
            "Bom dia a todos, vamos começar pelo status do lançamento.",
            "A versão para celular foi aprovada ontem, então podemos lançar na segunda-feira.",
            "Ótimo. O que ainda está bloqueando a versão web?",
            "Duas coisas. A página de pagamento precisa de uma revisão de segurança, e os artigos de ajuda ainda não foram traduzidos.",
            "Quem pode cuidar da revisão de segurança nesta semana?",
            "Eu posso fazer até quinta-feira se alguém cobrir o suporte na quarta.",
            "Eu cubro o suporte na quarta. Para as traduções, já temos um orçamento fechado.",
            "Então vamos contratar a agência do ano passado e pedir a entrega até o fim do mês.",
            "Combinado. Vamos nos reunir de novo na sexta para confirmar tudo.",
        ],
    },
];

pub struct Screenshot {
    pub id: &'static str,
    pub language: &'static str,
    /// `small` or `full-hd`.
    pub size: &'static str,
    pub width: u32,
    pub height: u32,
    pub font_px: u32,
    pub lines: &'static [&'static str],
}

impl Screenshot {
    pub fn reference(&self) -> String {
        self.lines.join("\n")
    }
    pub fn png(&self, assets: &Assets) -> PathBuf {
        assets.fixtures().join(format!("{}.png", self.id))
    }
}

const ARTICLE_EN: &[&str] = &[
    "Quarterly operations report",
    "Revenue grew 14% to $2.8 million, driven by annual plans.",
    "Support tickets fell from 1,240 to 980 after the new help center.",
    "The average first response time is now 3 hours and 12 minutes.",
    "Next steps: hire two engineers, renew the cloud contract,",
    "and publish the security audit before March 31.",
    "Contact: operations@example.com or +1 (555) 010-2030.",
    "Appendix A lists every vendor with its renewal date.",
    "Appendix B compares the three hosting offers we received.",
    "Risks: currency changes, a late audit and hiring delays.",
    "Owner: the operations team. Reviewed by finance on May 2.",
    "Version 3.4, approved for internal distribution only.",
];

const ARTICLE_PT: &[&str] = &[
    "Relatório trimestral de operações",
    "A receita cresceu 14% para R$ 2,8 milhões com os planos anuais.",
    "Os chamados de suporte caíram de 1.240 para 980 este trimestre.",
    "O tempo médio da primeira resposta agora é de 3 horas.",
    "Próximos passos: contratar dois engenheiros e renovar o contrato,",
    "e publicar a auditoria de segurança antes de 31 de março.",
    "Contato: operacoes@exemplo.com.br ou (11) 5555-0142.",
    "O apêndice A lista cada fornecedor com a data de renovação.",
    "O apêndice B compara as três propostas de hospedagem.",
    "Riscos: câmbio, auditoria atrasada e demora nas contratações.",
    "Responsável: equipe de operações. Revisado em 2 de maio.",
    "Versão 3.4, aprovada apenas para distribuição interna.",
];

pub const SCREENSHOTS: &[Screenshot] = &[
    Screenshot {
        id: "ocr-small-en",
        language: "en",
        size: "small",
        width: 800,
        height: 450,
        font_px: 15,
        lines: ARTICLE_EN.split_at(6).0,
    },
    Screenshot {
        id: "ocr-small-pt",
        language: "pt",
        size: "small",
        width: 800,
        height: 450,
        font_px: 15,
        lines: ARTICLE_PT.split_at(6).0,
    },
    Screenshot {
        id: "ocr-fullhd-en",
        language: "en",
        size: "full-hd",
        width: 1920,
        height: 1080,
        font_px: 22,
        lines: ARTICLE_EN,
    },
    Screenshot {
        id: "ocr-fullhd-pt",
        language: "pt",
        size: "full-hd",
        width: 1920,
        height: 1080,
        font_px: 22,
        lines: ARTICLE_PT,
    },
];

/// Writes every fixture into `<assets>/fixtures`.
pub fn generate(assets: &Assets) -> Result<()> {
    std::fs::create_dir_all(assets.fixtures())?;
    for speech in DICTATION.iter().chain(MEETINGS) {
        synthesize(speech, &speech.wav(assets))?;
        println!("wrote {}", speech.wav(assets).display());
    }
    for shot in SCREENSHOTS {
        draw(shot, &shot.png(assets))?;
        println!("wrote {}", shot.png(assets).display());
    }
    Ok(())
}

/// Speaks one line with one voice into its own file; switching voices inside
/// one prompt garbles the audio after the first switch.
const SPEAK: &str = r#"
param([string]$Spec)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech
$job = Get-Content -Raw -Encoding UTF8 $Spec | ConvertFrom-Json
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$synth.SelectVoice($job.voice)
$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)
$synth.SetOutputToWaveFile($job.out, $format)
$synth.Speak($job.line)
$synth.SetOutputToNull()
$synth.Dispose()
"#;

const DRAW: &str = r#"
param([string]$Spec)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$job = Get-Content -Raw -Encoding UTF8 $Spec | ConvertFrom-Json
$bitmap = New-Object System.Drawing.Bitmap($job.width, $job.height)
$g = [System.Drawing.Graphics]::FromImage($bitmap)
$g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
$g.Clear([System.Drawing.Color]::FromArgb(250, 250, 248))
$bar = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(38, 50, 72))
$g.FillRectangle($bar, 0, 0, $job.width, [int]($job.fontPx * 2.4))
$title = New-Object System.Drawing.Font('Segoe UI Semibold', [single]($job.fontPx * 1.25), [System.Drawing.GraphicsUnit]::Pixel)
$body = New-Object System.Drawing.Font('Segoe UI', [single]$job.fontPx, [System.Drawing.GraphicsUnit]::Pixel)
$ink = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(30, 30, 34))
$y = [int]($job.fontPx * 4)
for ($i = 0; $i -lt $job.lines.Count; $i++) {
    $font = if ($i -eq 0) { $title } else { $body }
    $g.DrawString($job.lines[$i], $font, $ink, [single]($job.fontPx * 2), [single]$y)
    $y += [int]($job.fontPx * 1.9)
}
$bitmap.Save($job.out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bitmap.Dispose()
"#;

fn run_script(script: &str, spec: serde_json::Value, out: &Path) -> Result<()> {
    let directory = std::env::temp_dir().join(format!("phorminx-fixture-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let script_path = directory.join("job.ps1");
    let spec_path = directory.join("job.json");
    std::fs::write(&script_path, script)?;
    std::fs::write(&spec_path, serde_json::to_vec(&spec)?)?;
    let status = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script_path)
        .arg(&spec_path)
        .status()
        .context("failed to start PowerShell")?;
    let _ = std::fs::remove_dir_all(&directory);
    ensure!(
        status.success() && out.is_file(),
        "failed to write {}",
        out.display()
    );
    Ok(())
}

/// Speaks each line separately and joins them with short pauses, after the
/// leading silence.
fn synthesize(speech: &Speech, out: &Path) -> Result<()> {
    let rate = 16_000_u32;
    let mut samples = vec![0.0_f32; (LEADING_SILENCE * f64::from(rate)) as usize];
    for (index, line) in speech.lines.iter().enumerate() {
        let part = out.with_extension(format!("part{index}.wav"));
        run_script(
            SPEAK,
            json!({"out": part, "voice": speech.voices[index % speech.voices.len()], "line": line}),
            &part,
        )?;
        let clip = read_wav(&part).with_context(|| format!("failed to read {}", part.display()))?;
        let _ = std::fs::remove_file(&part);
        ensure!(
            clip.sample_rate == rate,
            "the synthesizer wrote {} Hz audio",
            clip.sample_rate
        );
        samples.extend(clip.samples);
        samples.extend(std::iter::repeat_n(0.0, (rate as usize) * 35 / 100));
    }
    write_wav(
        out,
        &AudioClip {
            samples,
            sample_rate: rate,
        },
    )
    .with_context(|| format!("failed to write {}", out.display()))
}

fn draw(shot: &Screenshot, out: &Path) -> Result<()> {
    run_script(
        DRAW,
        json!({
            "out": out,
            "width": shot.width,
            "height": shot.height,
            "fontPx": shot.font_px,
            "lines": shot.lines,
        }),
        out,
    )
}
