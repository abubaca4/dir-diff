use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use clap::Parser;
use colored::*;
use indicatif::{HumanBytes, MultiProgress, ProgressBar, ProgressStyle};
use pathdiff::diff_paths;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use walkdir::WalkDir;

mod compare;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Папка 1 (исходная/старая)
    #[arg(required = true)]
    dir1: PathBuf,

    /// Папка 2 (целевая/новая)
    #[arg(required = true)]
    dir2: PathBuf,

    /// Папка 3 (выходная для разницы)
    #[arg(required = true)]
    dir3: PathBuf,

    /// Подробный вывод новых файлов
    #[arg(long = "add", short = 'a')]
    add: bool,

    /// Подробный вывод изменённых файлов
    #[arg(long = "replace", short = 'r')]
    replace: bool,

    /// Подробный вывод удалённых файлов
    #[arg(long = "del", short = 'd')]
    del: bool,

    /// Целевой порог схожести SSIM (0.0 - 1.0) для изображений (например, 0.85).
    /// Если не задан, требуется полное совпадение.
    #[arg(long = "ssim")]
    ssim: Option<f64>,

    /// Копировать в результирующую папку ТОЛЬКО добавленные файлы
    #[arg(long = "only-added", conflicts_with = "only_modified")]
    only_added: bool,

    /// Копировать в результирующую папку ТОЛЬКО изменённые файлы
    #[arg(long = "only-modified", conflicts_with = "only_added")]
    only_modified: bool,
}

/// Сканирует папку, обновляет прогресс-бар и возвращает HashMap путей.
fn scan_directory(dir: &Path, pb: ProgressBar) -> HashMap<PathBuf, PathBuf> {
    let mut files = HashMap::new();
    let mut count = 0;

    // Обходим папку
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let abs_path = entry.path().to_path_buf();

            // Оптимизация вывода: обновляем сообщение не каждый раз, чтобы не тратить CPU на строки
            if count % 100 == 0 {
                pb.set_message(abs_path.display().to_string());
            }
            count += 1;
            pb.tick();

            // Вычисляем относительный путь для корректного сопоставления
            if let Some(rel_path) = diff_paths(&abs_path, dir) {
                files.insert(rel_path, abs_path);
            }
        }
    }

    pb.finish_with_message(format!("Завершено: {}", dir.display()));
    files
}

fn main() {
    let args = Args::parse();

    // =========================================================================
    // ФАЗА 1: Построение дерева
    // =========================================================================
    println!(
        "{}",
        "\n[1/3] Построение дерева директорий...".cyan().bold()
    );

    let m = MultiProgress::new();
    let spinner_style = ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] {msg}")
        .unwrap()
        .tick_chars("⠁⠂⠄⡀⢀⠠⠐⠈ ");

    let pb1 = m.add(ProgressBar::new_spinner());
    pb1.set_style(spinner_style.clone());
    let pb2 = m.add(ProgressBar::new_spinner());
    pb2.set_style(spinner_style);

    // Параллельное сканирование двух папок с передачей спиннеров
    let (files1, files2) = rayon::join(
        || scan_directory(&args.dir1, pb1),
        || scan_directory(&args.dir2, pb2),
    );

    let mut added = Vec::new();
    let mut deleted = Vec::new();
    let mut candidate_modified = Vec::new();

    // Разделение файлов на категории
    for (rel_path, abs2) in &files2 {
        if let Some(abs1) = files1.get(rel_path) {
            candidate_modified.push((rel_path.clone(), abs1.clone(), abs2.clone()));
        } else {
            added.push(rel_path.clone());
        }
    }

    for rel_path in files1.keys() {
        if !files2.contains_key(rel_path) {
            deleted.push(rel_path.clone());
        }
    }

    let ssim_threshold = args.ssim;
    let skip_comparison = args.only_added && !args.replace;

    // =========================================================================
    // ФАЗА 2: Сравнение файлов
    // =========================================================================
    println!("{}", "\n[2/3] Сравнение файлов...".cyan().bold());

    let modified: Vec<PathBuf> = if skip_comparison {
        println!("{}", "Пропущено (выбран режим только добавленных)".dimmed());
        Vec::new()
    } else {
        let total_candidates = candidate_modified.len() as u64;
        let pb_compare = ProgressBar::new(total_candidates);
        pb_compare.set_style(
            ProgressStyle::with_template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} файлов ({eta})")
                .unwrap()
                .progress_chars("#>-"),
        );

        let res = candidate_modified
            .into_par_iter()
            .filter_map(|(rel_path, abs1, abs2)| {
                let is_identical = compare::are_files_identical(&abs1, &abs2, ssim_threshold);
                // Безопасный инкремент из любого потока
                pb_compare.inc(1);

                if !is_identical { Some(rel_path) } else { None }
            })
            .collect();

        pb_compare.finish_with_message("Сравнение завершено");
        res
    };

    // =========================================================================
    // ФАЗА 3: Процесс копирования
    // =========================================================================
    println!("{}", "\n[3/3] Подготовка к копированию...".cyan().bold());

    let mut raw_files_to_copy = Vec::new();
    let copy_all = !args.only_added && !args.only_modified;

    if copy_all || args.only_added {
        for rel_path in &added {
            raw_files_to_copy.push((rel_path, files2.get(rel_path).unwrap()));
        }
    }
    if copy_all || args.only_modified {
        for rel_path in &modified {
            raw_files_to_copy.push((rel_path, files2.get(rel_path).unwrap()));
        }
    }

    if raw_files_to_copy.is_empty() {
        println!("{}", "Нет файлов для копирования.".yellow());
    } else {
        // Оптимизация: Параллельно собираем размеры файлов ДО начала копирования,
        // чтобы получить точный общий вес и не дергать метадату повторно внутри самого копирования.
        let files_to_copy: Vec<(&PathBuf, &PathBuf, u64)> = raw_files_to_copy
            .into_par_iter()
            .map(|(rel, abs)| {
                let size = fs::metadata(abs).map(|m| m.len()).unwrap_or(0);
                (rel, abs, size)
            })
            .collect();

        let total_bytes: u64 = files_to_copy.iter().map(|(_, _, size)| size).sum();
        let total_files = files_to_copy.len() as u64;

        let pb_copy = ProgressBar::new(total_files);
        pb_copy.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.yellow/red}] {pos}/{len} файлов | Объем: {msg} ({eta})"
            )
            .unwrap()
            .progress_chars("#>-"),
        );

        // Форматируем начальное сообщение для нулей
        pb_copy.set_message(format!("0 B / {}", HumanBytes(total_bytes)));

        let copied_bytes = Arc::new(AtomicU64::new(0));

        files_to_copy
            .into_par_iter()
            .for_each(|(rel_path, abs_src, file_size)| {
                let dest_path = args.dir3.join(rel_path);

                if let Some(parent) = dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                if fs::copy(abs_src, dest_path).is_ok() {
                    // Атомарно обновляем счетчик скопированных байт
                    let current_bytes =
                        copied_bytes.fetch_add(file_size, Ordering::Relaxed) + file_size;

                    // Обновляем текст с объемом
                    pb_copy.set_message(format!(
                        "{} / {}",
                        HumanBytes(current_bytes),
                        HumanBytes(total_bytes)
                    ));
                }
                pb_copy.inc(1);
            });

        pb_copy.finish_with_message(format!("Скопировано: {}", HumanBytes(total_bytes)));
    }

    // =========================================================================
    // ВЫВОД РЕЗУЛЬТАТОВ (Без изменений)
    // =========================================================================
    println!();

    if args.add && !added.is_empty() {
        println!("{}", "--- ДОБАВЛЕННЫЕ ФАЙЛЫ ---".green().bold());
        for f in &added {
            println!("{}", f.display().to_string().green());
        }
        println!();
    }

    if args.replace && !modified.is_empty() {
        println!("{}", "--- ИЗМЕНЁННЫЕ ФАЙЛЫ ---".yellow().bold());
        for f in &modified {
            println!("{}", f.display().to_string().yellow());
        }
        println!();
    }

    if args.del && !deleted.is_empty() {
        println!("{}", "--- УДАЛЁННЫЕ ФАЙЛЫ ---".red().bold());
        for f in &deleted {
            println!("{}", f.display().to_string().red());
        }
        println!();
    }

    println!("=== СТАТИСТИКА ===");
    println!("{}", format!("Добавлено: {}", added.len()).green().bold());
    if skip_comparison {
        println!("{}", "Изменено: [ПРОПУЩЕНО]".yellow().bold());
    } else {
        println!(
            "{}",
            format!("Изменено: {}", modified.len()).yellow().bold()
        );
    }
    println!("{}", format!("Удалено: {}", deleted.len()).red().bold());
}
