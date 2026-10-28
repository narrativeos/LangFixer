//! B 档视觉抽取接线（`MULTIMODAL.md` §3 / §14 P1）
//!
//! 分工：**网络调用在这里**，解析 / 定位 / 置信度策略在 `media_core::vision`
//! （纯函数，可离线测）。这样视觉档不污染 media-core 的依赖面。
//!
//! 优先级铁律：A 档（结构层）取到文本就不用视觉档——结构层是真值，
//! 视觉档只在结构层为空（扁平位图 / 无文字层）时兜底。

use std::path::Path;
use std::sync::OnceLock;

use crate::settings::MediaSettings;
// 0.25 的编码走 ImageEncoder::write，方法调用需要 trait 在作用域内
use image::ImageEncoder as _;
use langfixer_llm::LlmConfig;
use langfixer_llm::client::{ChatMessage, LlmClient};
use langfixer_llm::templates::TemplateStore;
use media_core::{
    CarrierInfo, DEFAULT_VISION_SYSTEM, DEFAULT_VISION_USER, TextUnit, image_data_url,
    parse_vision_blocks, vision_mime, vision_units,
};
use tokio::sync::Semaphore;

/// 视觉档产物（job 里要落盘 `raw` 便于排查「模型到底说了什么」）
pub struct VisionOutcome {
    pub units: Vec<TextUnit>,
    pub warnings: Vec<String>,
    /// 实际使用的模型名（`media.vision_model` 为空时回退 `llm.model`）
    pub model: String,
    /// 提示词来源：`media_vision`（prompts 模板）或 `builtin`（内置默认）
    pub template: String,
    /// 模型原始响应（已 trim）
    pub raw: String,
}

/// 视觉抽取：读图 →（必要时压缩）→ data URL → 多模态 LLM → 文本块 → `TextUnit`
///
/// 返回 `Err(String)`：调用方（job 流水线）**降级为告警**而不是整个 job 失败——
/// 视觉档失败时结构层结果仍然可用。
pub async fn extract_vision_units(
    llm: &LlmConfig,
    media: &MediaSettings,
    prompts: &TemplateStore,
    lang: &str,
    image: &Path,
    carrier: &CarrierInfo,
) -> Result<VisionOutcome, String> {
    if !llm.enabled() {
        return Err("视觉档未就绪：未配置 LLM（设置 → LLM，或环境变量 LLM_API_KEY）".to_string());
    }
    let ext = image
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let Some(mime) = vision_mime(&ext) else {
        return Err(format!(
            "视觉端点不收 .{ext}：需先由适配器导出 png（PhotoCraft render_preview）"
        ));
    };
    let bytes = std::fs::read(image).map_err(|e| format!("读取图片失败：{e}"))?;
    // 上传前压缩：原图动辄 4–5MB，视觉端点的上传与视觉编码开销全在这段（见 compress_for_vision）
    let (bytes, mime, compress_warning) = compress_for_vision(
        bytes,
        mime,
        media.vision_max_edge,
        media.vision_max_bytes,
        media.vision_jpeg_quality,
    );

    // 提示词：prompts/media_vision_prompt.json 优先（可热更新），否则内置默认
    let (system, user, template) = match prompts.for_lang_cefr("media", "vision") {
        Some(t) => (
            t.system.clone(),
            t.render(&[("lang", lang)]),
            "media_vision".to_string(),
        ),
        None => (
            DEFAULT_VISION_SYSTEM.to_string(),
            DEFAULT_VISION_USER.replace("{{lang}}", lang),
            "builtin".to_string(),
        ),
    };

    // 视觉请求比讲解慢得多：超时取 max(llm, media)
    let mut lcfg = llm.clone();
    lcfg.timeout_secs = lcfg.timeout_secs.max(media.vision_timeout_secs);
    let client = LlmClient::new(&lcfg).with_model(&media.vision_model);
    let model = client.model().to_string();

    // 并发闸门：真实网关是**单槽串行**的，两个视觉请求一起打只会互相 503
    // （实测：同一批两张图并发跑，第二张必失败）。压完图再排队，别占着闸门压图。
    let mut warnings: Vec<String> = Vec::new();
    if let Some(w) = compress_warning {
        warnings.push(w);
    }
    let gate_started = std::time::Instant::now();
    let _permit = match vision_gate().acquire().await {
        Ok(p) => p,
        Err(_) => return Err("视觉档闸门已关闭（进程正在退出）".to_string()),
    };
    let gate_wait = gate_started.elapsed();
    if gate_wait.as_secs() >= 30 {
        warnings.push(format!(
            "视觉档排队等待：前面还有视觉任务（等了 {}s，闸门并发上限 {}）",
            gate_wait.as_secs(),
            vision_gate().available_permits() + 1
        ));
    }

    // 视觉档一次 503 就能让整个载体的抽取报废（真实评测实测：某轮载体1 整批 0 单元）。
    // 立场：LLM 服务基本稳定，503 多半只是**排队**——等就完了，不该判死；
    // 但非瞬时错误（未配置 / 4xx）不重试，那是要人修配置，重试只是拖慢失败。
    // 退避必须盖住网关的**排队窗口**：实测单槽网关跑完一次 2 分钟转录后，紧随其后的第二次
    // 视觉请求必 503，而隔几分钟单独探测同一张图 108s 就返回 200——旧的 2s/5s 退避够不着，
    // 连续两轮重演的载体 2 全灭。默认 20s/60s/120s/240s，
    // `GEC_MEDIA_VISION_RETRY_BACKOFF_SECS=0,0` 可关掉等待（测试用）。
    // 网关给了 `Retry-After` 就按它等（它知道自己排到哪），总等待由预算封顶（绝不无限等）。
    let backoff_secs = vision_retry_backoff();
    let budget = vision_retry_budget();
    let mut retries = 0usize;
    let mut waited = 0u64;
    let mut queued = false;
    let raw = loop {
        match client
            .complete_messages(
                &[
                    ChatMessage::system(&system),
                    ChatMessage::user_with_image(&user, &image_data_url(mime, &bytes)),
                ],
                0.0, // 转录要稳定，不要采样温度
            )
            .await
        {
            Ok(raw) => break raw,
            Err(e) if e.is_transient() => {
                queued = queued || e.is_queued();
                match next_wait(&backoff_secs, retries, e.retry_after_secs(), waited, budget) {
                    Some(wait) => {
                        tracing::warn!(
                            job_error = %e,
                            wait_secs = wait,
                            attempt = retries + 1,
                            "视觉档网关排队：退避后重试"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                        waited += wait;
                        retries += 1;
                    }
                    None => {
                        return Err(format!(
                            "视觉模型调用失败（{}已重试 {retries} 次、共等待 {waited}s；\
                             退避表 {backoff_secs:?}、预算 {budget}s：\
                             GEC_MEDIA_VISION_RETRY_BACKOFF_SECS / \
                             GEC_MEDIA_VISION_RETRY_BUDGET_SECS 可调）：{e}",
                            if queued {
                                "网关排队："
                            } else {
                                "瞬时故障："
                            }
                        ));
                    }
                }
            }
            Err(e) => return Err(format!("视觉模型调用失败：{e}")),
        }
    };
    if retries > 0 {
        warnings.push(format!(
            "视觉档{}重试 {retries} 次后成功（共 {} 次尝试、等待 {waited}s）",
            if queued {
                "网关排队"
            } else {
                "瞬时故障"
            },
            retries + 1
        ));
    }
    let (blocks, parse_warning) = parse_vision_blocks(&raw);
    warnings.extend(parse_warning.map(|w| vec![w]).unwrap_or_default());
    if blocks.is_empty() {
        warnings.push("视觉模型未返回任何文本块".to_string());
        return Ok(VisionOutcome {
            units: Vec::new(),
            warnings,
            model,
            template,
            raw: raw.trim().to_string(),
        });
    }
    let (units, mut unit_warnings) =
        vision_units(&blocks, carrier.width, carrier.height, media.min_confidence);
    warnings.append(&mut unit_warnings);
    Ok(VisionOutcome {
        units,
        warnings,
        model,
        template,
        raw: raw.trim().to_string(),
    })
}

/// 视觉档瞬时故障的退避表（秒）：长度即重试次数。
///
/// 默认 `20,60,120,240`——真实网关是单槽串行的，跑完一次长转录后有一段几十秒到两分钟的
/// 503 窗口，短退避（旧 2s/5s）只会连续撞墙。`GEC_MEDIA_VISION_RETRY_BACKOFF_SECS`
/// 逗号分隔可覆盖（测试设 `0,0` 免等待）；解析不出任何数字时回默认，绝不退化成「不重试」。
fn vision_retry_backoff() -> Vec<u64> {
    parse_backoff(
        std::env::var("GEC_MEDIA_VISION_RETRY_BACKOFF_SECS")
            .ok()
            .as_deref(),
    )
}

/// 视觉档重试的总等待预算（秒）：默认 900（15 分钟）
///
/// 视觉转录一次就是 100s+ 量级，排队窗口可以到几分钟；预算太小会把「再等一轮就有结果」
/// 的载体判死，太大则用户看着 job 卡住。`GEC_MEDIA_VISION_RETRY_BUDGET_SECS` 可覆盖。
fn vision_retry_budget() -> u64 {
    std::env::var("GEC_MEDIA_VISION_RETRY_BUDGET_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(900)
}

/// 重试决策（纯函数）：`Some(等待秒数)` 继续重试，`None` 就此收手
///
/// 网关给了 `Retry-After` 就按它等（它知道自己排到哪，猜的退避只会短），
/// 但总等待不得越过预算。与 `gec_core::llm` 的同名私有决策保持同一口径。
fn next_wait(
    backoff: &[u64],
    attempt: usize,
    retry_after: Option<u64>,
    waited: u64,
    budget: u64,
) -> Option<u64> {
    let step = *backoff.get(attempt)?;
    let wait = retry_after.unwrap_or(0).max(step);
    if waited + wait > budget {
        None
    } else {
        Some(wait)
    }
}

/// 视觉档并发闸门（进程级）：默认 **1**
///
/// 网关只有一个槽位时，并发视觉请求不会变快，只会互相把对方挤成 503
/// （实测同一批两张图并发跑，第二张必失败）。`GEC_MEDIA_VISION_CONCURRENCY` 可调
/// （多副本网关上 > 1 才有意义）。
fn vision_gate() -> &'static Semaphore {
    static GATE: OnceLock<Semaphore> = OnceLock::new();
    GATE.get_or_init(|| {
        let n = std::env::var("GEC_MEDIA_VISION_CONCURRENCY")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(1);
        Semaphore::new(n)
    })
}

/// 纯解析（不碰环境变量，免得并行测试互相踩）
fn parse_backoff(raw: Option<&str>) -> Vec<u64> {
    const DEFAULT: [u64; 4] = [20, 60, 120, 240];
    match raw {
        Some(v) => {
            let parsed: Vec<u64> = v
                .split(',')
                .filter_map(|s| s.trim().parse::<u64>().ok())
                .collect();
            if parsed.is_empty() {
                DEFAULT.to_vec()
            } else {
                parsed
            }
        }
        None => DEFAULT.to_vec(),
    }
}

/// 视觉上传前的压缩：长边降到 `max_edge`、体积压到 `max_bytes` 以下
///
/// 为什么必须做（本机网关 + 两张实拍告示实测）：原图 4.9MB / 3.6MB（长边 3000+）送整页
/// 转录要 110s+，紧随其后的请求必 503，退避加到 200s 仍救不回第二个载体；同一张图降到
/// 长边 2000px + JPEG q80 是 0.73MB / 0.52MB（约 7 倍）。视觉块用的是**归一化 bbox**，
/// 降采样不影响回写定位，所以这段只赚不亏（`MULTIMODAL.md` §9「体积/时长」的落地）。
///
/// 返回 `(bytes, mime, 告警)`：不需要压 / 解不开 / 压了反而更大时**原样返回**，
/// 但后两种会留告警——不能静默把「没压」读成「压过了」。
fn compress_for_vision(
    bytes: Vec<u8>,
    mime: &'static str,
    max_edge: u32,
    max_bytes: u64,
    quality: u8,
) -> (Vec<u8>, &'static str, Option<String>) {
    let img = match image::load_from_memory(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (
                bytes,
                mime,
                Some(format!("视觉上传前压缩跳过：图片解不开（{e}），原样上传")),
            );
        }
    };
    let w = img.width();
    let h = img.height();
    if (bytes.len() as u64) <= max_bytes && w.max(h) <= max_edge {
        return (bytes, mime, None); // 已经在预算内，不动
    }
    // 带 alpha 的（png 透明层）不能塞进 JPEG（会把透明填成黑），重编码只能走无损 PNG
    let has_alpha = img.has_alpha();
    let base_q = quality.clamp(30, 100);
    let mut q = base_q;
    let mut long_edge = max_edge.max(64);
    let mut out: Vec<u8> = Vec::new();
    let mut out_mime = mime;
    let mut out_dims = (w, h);
    for attempt in 0..6u32 {
        let cur = if w.max(h) > long_edge {
            img.resize(long_edge, long_edge, image::imageops::FilterType::Lanczos3)
        } else {
            img.clone()
        };
        out_dims = (cur.width(), cur.height());
        let mut buf: Vec<u8> = Vec::new();
        // 0.25 的编码器是 ImageEncoder::write_image（没有 write_to 那种便捷方法）
        let written = if has_alpha {
            out_mime = "image/png";
            let rgba = cur.to_rgba8();
            image::codecs::png::PngEncoder::new(&mut buf).write_image(
                rgba.as_raw(),
                cur.width(),
                cur.height(),
                image::ExtendedColorType::Rgba8,
            )
        } else {
            out_mime = "image/jpeg";
            let rgb = cur.to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, q).write_image(
                rgb.as_raw(),
                cur.width(),
                cur.height(),
                image::ExtendedColorType::Rgb8,
            )
        };
        match written {
            Ok(()) if (buf.len() as u64) <= max_bytes || attempt == 5 => {
                out = buf;
                break;
            }
            Ok(_) => {
                // 先降质量，质量见底再降边长：文字载体宁可少点像素也别糊成马赛克
                if q > 55 {
                    q = q.saturating_sub(15);
                } else {
                    long_edge = (long_edge / 2).max(512);
                    q = base_q;
                }
            }
            Err(e) => {
                return (
                    bytes,
                    mime,
                    Some(format!("视觉上传前压缩失败：{e}，原样上传")),
                );
            }
        }
    }
    let (in_len, out_len) = (bytes.len(), out.len());
    if out.is_empty() || out_len >= in_len {
        // 少见：小 png 重编码成无损反而更大，那就不套壳，但要说清没压
        return (
            bytes,
            mime,
            Some(format!(
                "视觉上传前压缩无收益（{} → {}），原样上传",
                fmt_bytes(in_len),
                fmt_bytes(out_len)
            )),
        );
    }
    let warning = format!(
        "视觉上传前压缩：{} {}×{} → {} {}×{}（{} q{q}）",
        fmt_bytes(in_len),
        w,
        h,
        fmt_bytes(out_len),
        out_dims.0,
        out_dims.1,
        if has_alpha { "PNG" } else { "JPEG" }
    );
    (out, out_mime, Some(warning))
}

fn fmt_bytes(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.2}MB", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}KB", n as f64 / 1_000.0)
    } else {
        format!("{n}B")
    }
}

#[cfg(test)]
mod tests {
    use super::{compress_for_vision, fmt_bytes, next_wait, parse_backoff};
    use image::ImageEncoder as _;

    fn jpeg(w: u32, h: u32, quality: u8) -> Vec<u8> {
        // 渐变而不是纯色：纯色 JPEG 小到失真（压不压都一样），测不出降采样的收益
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8])
        });
        let mut buf = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality)
            .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    #[test]
    fn 大图压到长边上限且显著变小() {
        let raw = jpeg(2600, 1300, 95);
        let (out, mime, warn) = compress_for_vision(raw.clone(), "image/jpeg", 1600, 200_000, 80);
        let back = image::load_from_memory(&out).unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(
            back.width().max(back.height()) <= 1600,
            "长边必须降到上限内：{}/{} > 1600",
            back.width(),
            back.height()
        );
        assert!(
            out.len() < raw.len(),
            "压缩必须真的变小：{} → {}",
            raw.len(),
            out.len()
        );
        let w = warn.expect("压过了就必须留告警（不能静默）");
        assert!(w.contains("视觉上传前压缩"), "{w}");
    }

    #[test]
    fn 预算内的小图原样返回不留告警() {
        let raw = jpeg(300, 200, 85);
        let (out, mime, warn) = compress_for_vision(raw.clone(), "image/jpeg", 2000, 1_500_000, 80);
        assert_eq!(out, raw, "预算内不该动原图");
        assert_eq!(mime, "image/jpeg");
        assert!(warn.is_none(), "没压就不该留告警：{warn:?}");
    }

    #[test]
    fn 解不开的图片原样上传但要留告警() {
        let raw = b"this is not an image".to_vec();
        let (out, mime, warn) = compress_for_vision(raw.clone(), "image/png", 2000, 1_500_000, 80);
        assert_eq!(out, raw, "解不开也得原样送，不能把载体判死");
        assert_eq!(mime, "image/png");
        assert!(
            warn.expect("解不开必须留告警").contains("解不开"),
            "告警要说清为什么没压"
        );
    }

    #[test]
    fn 字节数文案可读() {
        assert_eq!(fmt_bytes(4_949_911), "4.95MB");
        assert_eq!(fmt_bytes(725_352), "725KB");
        assert_eq!(fmt_bytes(812), "812B");
    }

    #[test]
    fn 默认退避盖住网关排队窗口() {
        let b = parse_backoff(None);
        assert_eq!(b.len(), 4, "默认四次重试（排队窗口可比单次转录还长）");
        assert!(
            b.iter().all(|s| *s >= 20),
            "短退避在单槽网关上必然连续撞 503（实测 2s/5s 全灭）：{b:?}"
        );
    }

    #[test]
    fn 退避表可覆盖_垃圾值回默认() {
        assert_eq!(parse_backoff(Some("0,0")), vec![0, 0], "测试要免等待");
        assert_eq!(parse_backoff(Some(" 30 , 90 ")), vec![30, 90], "空白要容忍");
        assert_eq!(
            parse_backoff(Some("abc, ,x")).len(),
            4,
            "全是垃圾值必须回默认，不能退化成不重试"
        );
    }

    #[test]
    fn 重试按网关给的等待走但不越预算() {
        let b = [20, 60, 120, 240];
        assert_eq!(next_wait(&b, 0, None, 0, 900), Some(20));
        assert_eq!(
            next_wait(&b, 0, Some(300), 0, 900),
            Some(300),
            "网关说排 300s 就等 300s（猜的退避只会短）"
        );
        assert_eq!(
            next_wait(&b, 2, None, 850, 900),
            None,
            "越过预算必须收手，不能把 job 挂死"
        );
        assert_eq!(
            next_wait(&b, 4, None, 0, 900),
            None,
            "退避表用完 = 次数用完"
        );
        assert_eq!(next_wait(&[], 0, None, 0, 900), None, "空表 = 不重试");
    }
}
