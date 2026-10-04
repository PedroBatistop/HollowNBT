#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod processor;

use eframe::egui;
use processor::{process_file, ProcessOptions, ProcessSummary};
use rfd::FileDialog;
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver},
    thread,
};

struct HollowApp {
    input_path: Option<PathBuf>,
    output_path: Option<PathBuf>,
    thickness: usize,
    transparent_depth: usize,
    preserve_falling: bool,
    crop: bool,
    preserve_block_entities: bool,
    extra_transparent: String,
    extra_falling: String,
    running: bool,
    log: Vec<String>,
    receiver: Option<Receiver<WorkerMessage>>,
    last_summary: Option<ProcessSummary>,
    last_error: Option<String>,
}

enum WorkerMessage {
    Log(String),
    Done(ProcessSummary),
    Error(String),
}

impl Default for HollowApp {
    fn default() -> Self {
        Self {
            input_path: None,
            output_path: None,
            thickness: 1,
            transparent_depth: 24,
            preserve_falling: true,
            crop: true,
            preserve_block_entities: true,
            extra_transparent: String::new(),
            extra_falling: String::new(),
            running: false,
            log: vec!["Selecione ou arraste um arquivo .nbt para começar.".to_string()],
            receiver: None,
            last_summary: None,
            last_error: None,
        }
    }
}

impl HollowApp {
    fn set_input(&mut self, path: PathBuf) {
        if path.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("nbt")) != Some(true) {
            self.last_error = Some("Selecione um arquivo com extensão .nbt.".to_string());
            return;
        }
        self.output_path = Some(default_output(&path));
        self.input_path = Some(path);
        self.last_error = None;
        self.last_summary = None;
    }

    fn start(&mut self, ctx: &egui::Context) {
        let Some(input) = self.input_path.clone() else {
            self.last_error = Some("Selecione um arquivo .nbt primeiro.".to_string());
            return;
        };
        let output = self.output_path.clone();
        let options = ProcessOptions {
            thickness: self.thickness.max(1),
            transparent_depth: self.transparent_depth,
            preserve_falling_supports: self.preserve_falling,
            crop_empty_space: self.crop,
            preserve_block_entities: self.preserve_block_entities,
            extra_transparent_hints: split_hints(&self.extra_transparent),
            extra_falling_hints: split_hints(&self.extra_falling),
        };

        self.running = true;
        self.last_error = None;
        self.last_summary = None;
        self.log.clear();
        self.log.push("Iniciando processamento...".to_string());

        let (ui_tx, ui_rx) = mpsc::channel::<WorkerMessage>();
        self.receiver = Some(ui_rx);
        let repaint = ctx.clone();

        thread::spawn(move || {
            let (log_tx, log_rx) = mpsc::channel::<String>();
            let forward_tx = ui_tx.clone();
            let repaint_logs = repaint.clone();
            let forwarder = thread::spawn(move || {
                while let Ok(line) = log_rx.recv() {
                    let _ = forward_tx.send(WorkerMessage::Log(line));
                    repaint_logs.request_repaint();
                }
            });

            let result = process_file(input, output, options, log_tx);
            let _ = forwarder.join();

            match result {
                Ok(summary) => {
                    let _ = ui_tx.send(WorkerMessage::Done(summary));
                }
                Err(err) => {
                    let _ = ui_tx.send(WorkerMessage::Error(format!("{err:#}")));
                }
            }
            repaint.request_repaint();
        });
    }

    fn drain_worker(&mut self) {
        let mut finished = false;
        if let Some(rx) = &self.receiver {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    WorkerMessage::Log(line) => self.log.push(line),
                    WorkerMessage::Done(summary) => {
                        self.log.push("Arquivo criado com sucesso.".to_string());
                        self.last_summary = Some(summary);
                        self.running = false;
                        finished = true;
                    }
                    WorkerMessage::Error(err) => {
                        self.log.push("O processamento terminou com erro.".to_string());
                        self.last_error = Some(err);
                        self.running = false;
                        finished = true;
                    }
                }
            }
        }
        if finished {
            self.receiver = None;
        }
    }
}

impl eframe::App for HollowApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_worker();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Drag & drop.
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            let path = file.path();
            if !path.as_os_str().is_empty() {
                self.set_input(path.to_path_buf());
                break;
            }
        }

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Hollow NBT");
            ui.label("Transforme estruturas Vanilla/Create em uma casca externa compacta.");
            ui.add_space(8.0);

            ui.group(|ui| {
                ui.label("Arquivo de entrada");
                ui.horizontal(|ui| {
                    let mut text = self
                        .input_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "Arraste um .nbt aqui ou clique em Procurar".to_string());
                    ui.add_sized(
                        [ui.available_width() - 105.0, 24.0],
                        egui::TextEdit::singleline(&mut text).interactive(false),
                    );
                    if ui.add_enabled(!self.running, egui::Button::new("Procurar...")) .clicked() {
                        if let Some(path) = FileDialog::new().add_filter("Minecraft NBT", &["nbt"]).pick_file() {
                            self.set_input(path);
                        }
                    }
                });

                ui.add_space(6.0);
                ui.label("Arquivo de saída");
                ui.horizontal(|ui| {
                    let mut out_text = self
                        .output_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.add_sized(
                        [ui.available_width() - 105.0, 24.0],
                        egui::TextEdit::singleline(&mut out_text).interactive(false),
                    );
                    if ui
                        .add_enabled(self.input_path.is_some() && !self.running, egui::Button::new("Salvar como..."))
                        .clicked()
                    {
                        let mut dialog = FileDialog::new().add_filter("Minecraft NBT", &["nbt"]);
                        if let Some(input) = &self.input_path {
                            if let Some(parent) = input.parent() {
                                dialog = dialog.set_directory(parent);
                            }
                            if let Some(default_name) = self.output_path.as_ref().and_then(|p| p.file_name()) {
                                dialog = dialog.set_file_name(default_name.to_string_lossy());
                            }
                        }
                        if let Some(path) = dialog.save_file() {
                            self.output_path = Some(path);
                        }
                    }
                });
            });

            ui.add_space(8.0);
            ui.columns(2, |cols| {
                cols[0].group(|ui| {
                    ui.heading("Casca");
                    ui.horizontal(|ui| {
                        ui.label("Espessura:");
                        ui.add_enabled(
                            !self.running,
                            egui::DragValue::new(&mut self.thickness).range(1..=32),
                        );
                        ui.label("bloco(s)");
                    });
                    ui.horizontal(|ui| {
                        ui.label("Visão através de vidro:");
                        ui.add_enabled(
                            !self.running,
                            egui::DragValue::new(&mut self.transparent_depth).range(0..=256),
                        );
                        ui.label("blocos");
                    });
                    ui.add_enabled_ui(!self.running, |ui| {
                        ui.checkbox(&mut self.crop, "Recortar espaços vazios das bordas");
                        ui.checkbox(
                            &mut self.preserve_block_entities,
                            "Preservar Block Entities mesmo escondidas",
                        );
                    });
                });

                cols[1].group(|ui| {
                    ui.heading("Física");
                    ui.add_enabled_ui(!self.running, |ui| {
                        ui.checkbox(
                            &mut self.preserve_falling,
                            "Preservar suporte de blocos com gravidade",
                        );
                    });
                    ui.small("Inclui areia, areia vermelha, gravel, concrete powder, bigornas e dragon egg.");
                });
            });

            ui.add_space(8.0);
            ui.collapsing("Configuração avançada de blocos modded", |ui| {
                ui.label("Transparentes/vazados extras, separados por vírgula ou linha:");
                ui.add_enabled(
                    !self.running,
                    egui::TextEdit::multiline(&mut self.extra_transparent)
                        .desired_rows(2)
                        .hint_text("ex.: framedblocks:, create:framed_glass"),
                );
                ui.label("Blocos com gravidade extras:");
                ui.add_enabled(
                    !self.running,
                    egui::TextEdit::multiline(&mut self.extra_falling)
                        .desired_rows(2)
                        .hint_text("ex.: nomedomod:falling_block"),
                );
            });

            ui.add_space(10.0);
            let can_run = self.input_path.is_some() && !self.running;
            if ui
                .add_enabled(can_run, egui::Button::new("PROCESSAR NBT").min_size([180.0, 36.0].into()))
                .clicked()
            {
                self.start(ui.ctx());
            }
            if self.running {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Processando. Estruturas grandes podem exigir bastante memória.");
                });
            }

            if let Some(err) = &self.last_error {
                ui.add_space(8.0);
                ui.colored_label(ui.visuals().error_fg_color, err);
            }

            if let Some(s) = &self.last_summary {
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.heading("Resultado");
                    ui.label(format!(
                        "Blocos: {} → {}  |  removidos: {} ({:.2}%)",
                        s.original_blocks, s.kept_blocks, s.removed_blocks, s.reduction_percent
                    ));
                    ui.label(format!(
                        "Tamanho: {}×{}×{} → {}×{}×{}",
                        s.original_size[0], s.original_size[1], s.original_size[2],
                        s.final_size[0], s.final_size[1], s.final_size[2]
                    ));
                    ui.label(format!(
                        "Transparentes externos: {} | suportes preservados: {}",
                        s.transparent_external, s.falling_supports_preserved
                    ));
                    if s.unsupported_falling_blocks > 0 {
                        ui.label(format!(
                            "Atenção: {} bloco(s) com gravidade já estavam sem suporte no NBT original.",
                            s.unsupported_falling_blocks
                        ));
                    }
                    ui.label(format!("Saída: {}", s.output.display()));
                });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.label("Log");
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            });
        });
    }
}

fn split_hints(text: &str) -> Vec<String> {
    text.split(|c| c == ',' || c == ';' || c == '\n' || c == '\r')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn default_output(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|x| x.to_str()).unwrap_or("structure");
    parent.join(format!("{stem}_hollow.nbt"))
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 700.0])
            .with_min_inner_size([640.0, 560.0])
            .with_drag_and_drop(true),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "Hollow NBT",
        options,
        Box::new(|_cc| Ok(Box::new(HollowApp::default()))),
    )
}
