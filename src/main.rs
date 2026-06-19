use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use clap::Parser;
use colored::*;
use pathdiff::diff_paths;
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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

/// Сканирует папку и возвращает HashMap, где ключ - относительный путь, значение - абсолютный.
fn scan_directory(dir: &Path) -> HashMap<PathBuf, PathBuf> {
    let mut files = HashMap::new();
    // Обходим папку
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let abs_path = entry.path().to_path_buf();
            // Вычисляем относительный путь для корректного сопоставления
            if let Some(rel_path) = diff_paths(&abs_path, dir) {
                files.insert(rel_path, abs_path);
            }
        }
    }
    files
}

fn main() {
    let args = Args::parse();

    // 1. Оптимизация: Параллельное сканирование двух папок
    let (files1, files2) =
        rayon::join(|| scan_directory(&args.dir1), || scan_directory(&args.dir2));

    let mut added = Vec::new();
    let mut deleted = Vec::new();
    let mut candidate_modified = Vec::new();

    // 2. Разделение файлов на категории
    // Проверяем файлы из Папки 2 (добавленные или изменённые)
    for (rel_path, abs2) in &files2 {
        if let Some(abs1) = files1.get(rel_path) {
            // Файл есть в обеих папках, нужен анализ
            candidate_modified.push((rel_path.clone(), abs1.clone(), abs2.clone()));
        } else {
            // Файла не было в Папке 1
            added.push(rel_path.clone());
        }
    }

    // Проверяем файлы из Папки 1 (удалённые)
    for rel_path in files1.keys() {
        if !files2.contains_key(rel_path) {
            deleted.push(rel_path.clone());
        }
    }

    let ssim_threshold = args.ssim;

    // 3. Оптимизация: Параллельное сравнение файлов (нагружает все ядра процессора)
    let modified: Vec<PathBuf> = candidate_modified
        .into_par_iter()
        .filter_map(|(rel_path, abs1, abs2)| {
            if !compare::are_files_identical(&abs1, &abs2, ssim_threshold) {
                Some(rel_path)
            } else {
                None
            }
        })
        .collect();

    // 4. Формируем список для параллельного копирования
    let mut files_to_copy = Vec::new();

    // Если ни один из флагов не указан, копируем всё (стандартное поведение)
    let copy_all = !args.only_added && !args.only_modified;

    if copy_all || args.only_added {
        for rel_path in &added {
            files_to_copy.push((rel_path, files2.get(rel_path).unwrap()));
        }
    }

    if copy_all || args.only_modified {
        for rel_path in &modified {
            files_to_copy.push((rel_path, files2.get(rel_path).unwrap()));
        }
    }

    // Параллельное копирование в Папку 3
    files_to_copy
        .into_par_iter()
        .for_each(|(rel_path, abs_src)| {
            let dest_path = args.dir3.join(rel_path);

            if let Some(parent) = dest_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::copy(abs_src, dest_path);
        });

    // 5. Вывод результатов в консоль
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

    // Полная статистика (выводится всегда)
    println!("=== СТАТИСТИКА ===");
    println!("{}", format!("Добавлено: {}", added.len()).green().bold());
    println!(
        "{}",
        format!("Изменено: {}", modified.len()).yellow().bold()
    );
    println!("{}", format!("Удалено: {}", deleted.len()).red().bold());
}
