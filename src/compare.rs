use std::fs;
use std::io::{self, Read};
use std::path::Path;
use image_compare::Algorithm;

/// Единая точка входа для сравнения двух файлов.
pub fn are_files_identical(path1: &Path, path2: &Path, ssim_threshold: Option<f64>) -> bool {
    let ext = path1
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let is_image = matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff"
    );

    if is_image {
        // Оптимизация: если файлы совпадают побайтово на диске (одинаковый размер и хэш),
        // значит и изображения 100% идентичны. Это экономит время на декодирование и вычисление SSIM.
        if compare_base(path1, path2) {
            return true;
        }

        match compare_images(path1, path2, ssim_threshold) {
            Ok(is_identical) => return is_identical,
            Err(_) => {
                // В случае ошибки декодирования (битый файл) считаем их разными
                return false;
            }
        }
    }

    // Базовая процедура для не-изображений
    compare_base(path1, path2)
}

/// Сравнение изображений (по SSIM или полное попиксельное)
fn compare_images(path1: &Path, path2: &Path, ssim_threshold: Option<f64>) -> Result<bool, image::ImageError> {
    if let Some(threshold) = ssim_threshold {
        // SSIM сравнение (RGB8 используется для лучшего учета цветного текста/деталей)
        let img1 = image::open(path1)?.into_rgb8();
        let img2 = image::open(path2)?.into_rgb8();

        // SSIM требует одинакового разрешения
        if img1.dimensions() != img2.dimensions() {
            return Ok(false);
        }

        match image_compare::rgb_similarity_structure(&Algorithm::MSSIMSimple, &img1, &img2) {
            // Если индекс сходства больше либо равен целевому порогу, считаем их одинаковыми
            Ok(result) => Ok(result.score >= threshold),
            Err(_) => Ok(false),
        }
    } else {
        // Старое поведение: точное сравнение RGBA буферов
        let img1 = image::open(path1)?.into_rgba8();
        let img2 = image::open(path2)?.into_rgba8();

        Ok(img1 == img2)
    }
}

/// Стандартное сравнение с предварительной проверкой размера и параллельным хэшированием
fn compare_base(path1: &Path, path2: &Path) -> bool {
    // 1. Оптимизация: сравниваем размер файлов
    let meta1 = fs::metadata(path1);
    let meta2 = fs::metadata(path2);

    if let (Ok(m1), Ok(m2)) = (meta1, meta2) {
        if m1.len() != m2.len() {
            return false;
        }
    } else {
        return false; // Ошибка доступа
    }

    // 2. Параллельный подсчет хэшей
    let (hash1_result, hash2_result) = rayon::join(|| hash_file(path1), || hash_file(path2));

    match (hash1_result, hash2_result) {
        (Ok(h1), Ok(h2)) => h1 == h2,
        _ => false,
    }
}

/// Подсчет хэша Blake3 потоковым чтением
fn hash_file(path: &Path) -> io::Result<blake3::Hash> {
    let mut file = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();

    // Чтение блоками по 64 КБ
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(hasher.finalize())
}