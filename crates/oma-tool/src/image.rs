//! 图片头部解析：只读取元数据，不做解码。
//!
//! `read` 工具要告诉模型「这是什么图」以及「能不能看图」。为此只需要宽高、
//! 通道数、是否带 alpha 与 MIME，这些都在头部几十字节内，无需引入完整解码器。
//! 解析失败不是错误：调用方保留 MIME 与体积，仍然能给出有用的描述。

/// 单张图片的元数据
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMeta {
    pub mime_type: String,
    pub width:     u32,
    pub height:    u32,
    /// 通道数：灰度 1、灰度+alpha 2、RGB 3、RGBA 4
    pub channels:  u8,
    pub has_alpha: bool,
}

impl ImageMeta {
    /// 人类可读的色彩模式，供工具输出直接引用
    pub fn color_mode(&self) -> &'static str {
        match (self.channels, self.has_alpha) {
            (1, _) => "grayscale",
            (2, _) => "grayscale+alpha",
            (3, _) => "RGB",
            (_, true) => "RGBA",
            _ => "RGB",
        }
    }

    /// 「256x256 RGBA」形态的紧凑描述
    pub fn describe(&self) -> String {
        format!("{}x{} {}", self.width, self.height, self.color_mode())
    }
}

/// 把图片格式名映射为 MIME 类型。
pub fn mime_for_format(format: &str) -> &'static str {
    match format {
        "png" => "image/png",
        "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" => "image/tiff",
        _ => "application/octet-stream",
    }
}

/// 按文件头解析图片元数据；格式不受支持或头部损坏时返回 None。
pub fn parse(bytes: &[u8]) -> Option<ImageMeta> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        parse_png(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        parse_jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        parse_gif(bytes)
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        parse_webp(bytes)
    } else if bytes.starts_with(b"BM") {
        parse_bmp(bytes)
    } else if bytes.starts_with(b"II\x2A\x00") || bytes.starts_with(b"MM\x00\x2A") {
        parse_tiff(bytes)
    } else {
        None
    }
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// PNG：IHDR 紧跟 8 字节签名 + 4 字节长度 + 4 字节类型。
/// 色彩类型 4 = 灰度+alpha，6 = RGBA，其余带 alpha 的 PNG 只有 tRNS 块，此处不追。
fn parse_png(bytes: &[u8]) -> Option<ImageMeta> {
    const IHDR_AT: usize = 16;
    let width = be_u32(bytes, IHDR_AT)?;
    let height = be_u32(bytes, IHDR_AT + 4)?;
    let (channels, has_alpha) = match *bytes.get(IHDR_AT + 9)? {
        0 => (1, false),
        2 => (3, false),
        3 => (1, false), // 调色板：每像素索引 1 通道
        4 => (2, true),
        6 => (4, true),
        _ => return None,
    };
    Some(ImageMeta {
        mime_type: "image/png".to_string(),
        width,
        height,
        channels,
        has_alpha,
    })
}

/// GIF：逻辑屏幕描述符里的宽高为小端 u16。
fn parse_gif(bytes: &[u8]) -> Option<ImageMeta> {
    let width = le_u16(bytes, 6)? as u32;
    let height = le_u16(bytes, 8)? as u32;
    Some(ImageMeta {
        mime_type: "image/gif".to_string(),
        width,
        height,
        channels: 1,                             // 索引色
        has_alpha: bytes.starts_with(b"GIF89a"), // 89a 才有透明扩展
    })
}

fn parse_bmp(bytes: &[u8]) -> Option<ImageMeta> {
    let width = le_u32(bytes, 18)? as i32;
    let height = le_u32(bytes, 22)? as i32;
    let depth = le_u16(bytes, 28)?;
    let channels = match depth {
        32 => 4,
        24 => 3,
        16 => 3,
        8 | 4 | 1 => 1,
        _ => return None,
    };
    Some(ImageMeta {
        mime_type: "image/bmp".to_string(),
        width: width.unsigned_abs(),
        height: height.unsigned_abs(), // 负高度表示自顶向下存储
        channels,
        has_alpha: depth == 32,
    })
}

/// TIFF：宽高存放在 IFD 条目里，先读首个 IFD 偏移再线性查找两个标签。
fn parse_tiff(bytes: &[u8]) -> Option<ImageMeta> {
    let little = bytes.starts_with(b"II");
    let read_u16 = |at: usize| if little { le_u16(bytes, at) } else { be_u16(bytes, at) };
    let read_u32 = |at: usize| if little { le_u32(bytes, at) } else { be_u32(bytes, at) };

    let ifd = read_u32(4)? as usize;
    let count = read_u16(ifd)? as usize;
    let (mut width, mut height) = (None, None);
    for i in 0..count {
        // 每个条目 12 字节：tag(2) type(2) count(4) value(4)
        let entry = ifd + 2 + i * 12;
        let tag = read_u16(entry)?;
        // 宽高按 SHORT 或 LONG 存储，取值都落在 value 字段前 4 字节
        let value = read_u32(entry + 8)?;
        match tag {
            0x0100 => width = Some(value),
            0x0101 => height = Some(value),
            _ => {}
        }
        if width.is_some() && height.is_some() {
            break;
        }
    }
    let channels = match read_u16(ifd + 2 + count * 12)? {
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        _ => 3,
    };
    Some(ImageMeta {
        mime_type: "image/tiff".to_string(),
        width: width?,
        height: height?,
        channels,
        has_alpha: channels == 2 || channels == 4,
    })
}

/// JPEG：扫描段直到 SOFn，其中记录宽高与分量数。
/// 带 alpha 的 JPEG 不存在（JPEG 无 alpha 通道）。
fn parse_jpeg(bytes: &[u8]) -> Option<ImageMeta> {
    let mut i = 2; // 跳过 SOI
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1; // 填充字节：继续找下一个标记
            continue;
        }
        let marker = bytes[i + 1];
        // 无参数标记：RSTn、TEM、以及连续标记
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 || marker == 0xFF {
            i += 2;
            continue;
        }
        let seg_len = be_u16(bytes, i + 2)? as usize;
        if seg_len < 2 {
            return None;
        }
        // SOF0..SOF15，排除 DHT(C4)/JPG(C8)/DAC(CC) 三个非 SOFn
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let height = be_u16(bytes, i + 5)? as u32;
            let width = be_u16(bytes, i + 7)? as u32;
            let channels = *bytes.get(i + 9)?;
            return Some(ImageMeta {
                mime_type: "image/jpeg".to_string(),
                width,
                height,
                channels,
                has_alpha: false,
            });
        }
        i += 2 + seg_len;
    }
    None
}

/// WebP：三种子格式（VP8 / VP8L / VP8X）各自的宽高位置不同。
fn parse_webp(bytes: &[u8]) -> Option<ImageMeta> {
    let chunk = bytes.get(12..16)?;
    // VP8X 扩展格式带 alpha 标志位
    if chunk == b"VP8X" {
        let flags = *bytes.get(20)?;
        let width = 1 + u32::from_le_bytes([*bytes.get(24)?, *bytes.get(25)?, *bytes.get(26)?, 0]);
        let height = 1 + u32::from_le_bytes([*bytes.get(27)?, *bytes.get(28)?, *bytes.get(29)?, 0]);
        return Some(ImageMeta {
            mime_type: "image/webp".to_string(),
            width,
            height,
            channels: 4,
            has_alpha: flags & 0x10 != 0,
        });
    }
    // 有损 VP8：13..15 为帧起始码，宽高为 14 位有符号
    if chunk == b"VP8 " {
        let (w, h) = (le_u16(bytes, 26)? as u32 & 0x3FFF, le_u16(bytes, 28)? as u32 & 0x3FFF);
        return Some(ImageMeta {
            mime_type: "image/webp".to_string(),
            width:     w,
            height:    h,
            channels:  3,
            has_alpha: false,
        });
    }
    // 无损 VP8L：14 位宽高打包在签名字节之后的 4 字节里
    if chunk == b"VP8L" {
        if *bytes.get(20)? != 0x2F {
            return None; // 缺少 VP8L 签名
        }
        let packed = le_u32(bytes, 21)?;
        return Some(ImageMeta {
            mime_type: "image/webp".to_string(),
            width:     (packed & 0x3FFF) + 1,
            height:    ((packed >> 14) & 0x3FFF) + 1,
            channels:  4,
            has_alpha: true, // VP8L 允许每像素 alpha
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 最小 PNG 头：签名 + IHDR（宽 256、高 128、8 位、色彩类型 6 = RGBA）
    fn png_header(width: u32, height: u32, color_type: u8) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes()); // IHDR 长度
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&width.to_be_bytes());
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&[8, color_type, 0, 0, 0]);
        v
    }

    #[test]
    fn test_png_metadata() {
        let meta = parse(&png_header(256, 128, 6)).unwrap();
        assert_eq!(meta.mime_type, "image/png");
        assert_eq!((meta.width, meta.height), (256, 128));
        assert_eq!(meta.channels, 4);
        assert!(meta.has_alpha);
        assert_eq!(meta.describe(), "256x128 RGBA");

        // 色彩类型 2 = RGB，无 alpha
        let rgb = parse(&png_header(3, 4, 2)).unwrap();
        assert_eq!((rgb.channels, rgb.has_alpha), (3, false));
        assert_eq!(rgb.color_mode(), "RGB");
        // 色彩类型 0 = 灰度
        let gray = parse(&png_header(1, 1, 0)).unwrap();
        assert_eq!(gray.color_mode(), "grayscale");
    }

    #[test]
    fn test_gif_metadata() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());
        gif.extend_from_slice(&[0xF7, 0, 0]);
        let meta = parse(&gif).unwrap();
        assert_eq!(meta.mime_type, "image/gif");
        assert_eq!((meta.width, meta.height), (640, 480));
        assert!(meta.has_alpha, "GIF89a carries a transparency extension");
    }

    #[test]
    fn test_bmp_metadata() {
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0; 16]); // 文件头其余字段无所谓
        bmp.extend_from_slice(&64u32.to_le_bytes()); // width
        bmp.extend_from_slice(&(-32i32).to_le_bytes()); // height 为负 = 自顶向下
        bmp.extend_from_slice(&[0, 0]); // planes
        bmp.extend_from_slice(&32u16.to_le_bytes()); // 每像素 32 位
        let meta = parse(&bmp).unwrap();
        assert_eq!(meta.mime_type, "image/bmp");
        assert_eq!((meta.width, meta.height), (64, 32), "negative height means top-down");
        assert!(meta.has_alpha);
    }

    #[test]
    fn test_jpeg_metadata() {
        // SOI + APP0(JFIF) + SOF0（高 480、宽 640、3 分量）
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0, 16]);
        jpeg.extend_from_slice(b"JFIF\0");
        jpeg.extend_from_slice(&[0; 9]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8]);
        jpeg.extend_from_slice(&480u16.to_be_bytes());
        jpeg.extend_from_slice(&640u16.to_be_bytes());
        jpeg.push(3);
        jpeg.extend_from_slice(&[0; 10]);
        let meta = parse(&jpeg).unwrap();
        assert_eq!(meta.mime_type, "image/jpeg");
        assert_eq!((meta.width, meta.height), (640, 480));
        assert!(!meta.has_alpha, "JPEG never carries an alpha channel");
    }

    #[test]
    fn test_webp_lossless_and_unknown() {
        // VP8L：签名字节 0x2F 后紧跟打包的 14 位宽 / 14 位高
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&0u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(b"VP8L");
        webp.extend_from_slice(&5u32.to_le_bytes());
        webp.push(0x2F);
        let packed: u32 = ((128u32 - 1) << 14) | (256 - 1);
        webp.extend_from_slice(&packed.to_le_bytes());
        let meta = parse(&webp).unwrap();
        assert_eq!(meta.mime_type, "image/webp");
        assert_eq!((meta.width, meta.height), (256, 128));

        // 无法识别的数据与截断头部都必须返回 None 而不是 panic
        assert!(parse(b"not an image at all").is_none());
        assert!(parse(&[]).is_none());
        assert!(parse(&png_header(1, 1, 6)[..20]).is_none(), "truncated IHDR");
        assert!(parse(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 2]).is_none(), "JPEG without SOFn");
        assert!(parse(&webp[..20]).is_none(), "truncated WebP chunk");
        assert!(
            parse(b"II\x2A\x00\x08\x00\x00\x00\x01\x00").is_none(),
            "truncated TIFF IFD"
        );
        // 有 BMP 签名但缺维度字段
        assert!(parse(b"BM\x00\x00").is_none());
    }

    #[test]
    fn test_base64_encode() {
        use crate::base64_encode;
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // 任一字节值都不能被丢掉或错位
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_encode(&all).len(), 344);
    }
}
