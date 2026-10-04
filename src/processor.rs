use anyhow::{anyhow, bail, Context, Result};
use fastnbt::Value;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

#[derive(Debug, Clone)]
pub struct ProcessOptions {
    pub thickness: usize,
    pub transparent_depth: usize,
    pub preserve_falling_supports: bool,
    pub crop_empty_space: bool,
    pub preserve_block_entities: bool,
    pub extra_transparent_hints: Vec<String>,
    pub extra_falling_hints: Vec<String>,
}

impl Default for ProcessOptions {
    fn default() -> Self {
        Self {
            thickness: 1,
            transparent_depth: 24,
            preserve_falling_supports: true,
            crop_empty_space: true,
            preserve_block_entities: true,
            extra_transparent_hints: Vec::new(),
            extra_falling_hints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProcessSummary {
    pub input: PathBuf,
    pub output: PathBuf,
    pub original_size: [i32; 3],
    pub final_size: [i32; 3],
    pub original_blocks: usize,
    pub kept_blocks: usize,
    pub removed_blocks: usize,
    pub reduction_percent: f64,
    pub transparent_external: usize,
    pub falling_supports_preserved: usize,
    pub unsupported_falling_blocks: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Structure {
    size: Vec<i32>,
    #[serde(default)]
    entities: Vec<Value>,
    blocks: Vec<Block>,
    palette: Vec<PaletteEntry>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Block {
    pos: Vec<i32>,
    state: i32,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaletteEntry {
    #[serde(rename = "Name")]
    name: String,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: [i32; 3],
    max: [i32; 3],
}

impl Bounds {
    fn size(self) -> [usize; 3] {
        [
            (self.max[0] - self.min[0] + 1) as usize,
            (self.max[1] - self.min[1] + 1) as usize,
            (self.max[2] - self.min[2] + 1) as usize,
        ]
    }
}

const DIR6: [[i32; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

const AIR_NAMES: [&str; 4] = [
    "minecraft:air",
    "minecraft:cave_air",
    "minecraft:void_air",
    "minecraft:structure_void",
];

const DEFAULT_SEE_THROUGH_HINTS: [&str; 25] = [
    "glass",
    "pane",
    "window",
    "ice",
    "water",
    "leaves",
    "web",
    "bars",
    "fence",
    "wall",
    "door",
    "trapdoor",
    "slab",
    "stairs",
    "chain",
    "ladder",
    "rail",
    "carpet",
    "sign",
    "torch",
    "button",
    "pressure_plate",
    "vine",
    "flower",
    "sapling",
];

const FALLING_EXACT: [&str; 9] = [
    "minecraft:sand",
    "minecraft:red_sand",
    "minecraft:gravel",
    "minecraft:suspicious_sand",
    "minecraft:suspicious_gravel",
    "minecraft:anvil",
    "minecraft:chipped_anvil",
    "minecraft:damaged_anvil",
    "minecraft:dragon_egg",
];

fn send(tx: &Sender<String>, msg: impl Into<String>) {
    let _ = tx.send(msg.into());
}

fn is_air(name: &str) -> bool {
    AIR_NAMES.iter().any(|x| *x == name)
}

fn is_see_through(name: &str, extra: &[String]) -> bool {
    if is_air(name) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    DEFAULT_SEE_THROUGH_HINTS
        .iter()
        .any(|hint| lower.contains(hint))
        || extra.iter().any(|hint| {
            let hint = hint.trim().to_ascii_lowercase();
            !hint.is_empty() && lower.contains(&hint)
        })
}

fn is_falling(name: &str, extra: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    FALLING_EXACT.iter().any(|x| *x == lower)
        || lower.ends_with("_concrete_powder")
        || extra.iter().any(|hint| {
            let hint = hint.trim().to_ascii_lowercase();
            !hint.is_empty() && lower.contains(&hint)
        })
}

fn output_path_for(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("structure");
    let parent = input.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}_hollow.nbt"))
}

fn read_nbt_bytes(path: &Path) -> Result<Vec<u8>> {
    let raw = fs::read(path).with_context(|| format!("Falha ao ler {}", path.display()))?;
    if raw.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(raw.as_slice());
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .context("Falha ao descompactar o GZip do NBT")?;
        Ok(out)
    } else {
        Ok(raw)
    }
}

fn write_gzip_nbt(path: &Path, structure: &Structure) -> Result<()> {
    let nbt = fastnbt::to_bytes(structure).context("Falha ao serializar NBT")?;
    let file = fs::File::create(path)
        .with_context(|| format!("Falha ao criar {}", path.display()))?;
    let mut encoder = GzEncoder::new(file, Compression::default());
    encoder
        .write_all(&nbt)
        .context("Falha ao comprimir NBT")?;
    encoder.finish().context("Falha ao finalizar GZip")?;
    Ok(())
}

fn validate_structure(s: &Structure) -> Result<[i32; 3]> {
    if s.size.len() != 3 {
        bail!("Campo size inválido: esperado 3 valores, encontrado {}", s.size.len());
    }
    let size = [s.size[0], s.size[1], s.size[2]];
    if size.iter().any(|v| *v <= 0) {
        bail!("Tamanho inválido no NBT: {size:?}");
    }
    for (i, b) in s.blocks.iter().enumerate() {
        if b.pos.len() != 3 {
            bail!("Bloco #{i} possui pos inválido");
        }
        if b.state < 0 || b.state as usize >= s.palette.len() {
            bail!("Bloco #{i} referencia palette inválida: {}", b.state);
        }
    }
    Ok(size)
}

fn occupied_bounds(structure: &Structure) -> Result<Bounds> {
    let mut iter = structure.blocks.iter().filter(|b| {
        let state = b.state as usize;
        state < structure.palette.len() && !is_air(&structure.palette[state].name)
    });

    let first = iter
        .next()
        .ok_or_else(|| anyhow!("A estrutura não possui blocos sólidos"))?;
    let mut min = [first.pos[0], first.pos[1], first.pos[2]];
    let mut max = min;

    for b in iter {
        for axis in 0..3 {
            min[axis] = min[axis].min(b.pos[axis]);
            max[axis] = max[axis].max(b.pos[axis]);
        }
    }
    Ok(Bounds { min, max })
}

#[inline]
fn cell_index(x: usize, y: usize, z: usize, sx: usize, sz: usize) -> usize {
    (y * sz + z) * sx + x
}

#[inline]
fn padded_index(x: usize, y: usize, z: usize, px: usize, pz: usize) -> usize {
    (y * pz + z) * px + x
}

fn checked_volume(size: [usize; 3]) -> Result<usize> {
    size[0]
        .checked_mul(size[1])
        .and_then(|v| v.checked_mul(size[2]))
        .ok_or_else(|| anyhow!("Estrutura grande demais para indexação"))
}

fn local_pos(block: &Block, bounds: Bounds) -> [usize; 3] {
    [
        (block.pos[0] - bounds.min[0]) as usize,
        (block.pos[1] - bounds.min[1]) as usize,
        (block.pos[2] - bounds.min[2]) as usize,
    ]
}

fn build_cells(structure: &Structure, bounds: Bounds) -> Result<(Vec<i32>, Vec<usize>)> {
    let [sx, sy, sz] = bounds.size();
    let volume = checked_volume([sx, sy, sz])?;
    let mut cells = vec![-1_i32; volume];
    let mut solid_block_indices = Vec::with_capacity(structure.blocks.len());

    for (block_idx, block) in structure.blocks.iter().enumerate() {
        let state = block.state as usize;
        if is_air(&structure.palette[state].name) {
            continue;
        }
        let [x, y, z] = local_pos(block, bounds);
        if x >= sx || y >= sy || z >= sz {
            bail!("Coordenada de bloco fora do bounding box calculado");
        }
        let idx = cell_index(x, y, z, sx, sz);
        if cells[idx] >= 0 {
            bail!(
                "O NBT contém mais de um bloco na mesma posição ({}, {}, {})",
                block.pos[0],
                block.pos[1],
                block.pos[2]
            );
        }
        if block_idx > i32::MAX as usize {
            bail!("Quantidade de blocos excede o limite desta versão");
        }
        cells[idx] = block_idx as i32;
        solid_block_indices.push(block_idx);
    }
    Ok((cells, solid_block_indices))
}

fn exterior_air_map(cells: &[i32], size: [usize; 3]) -> Result<Vec<u8>> {
    let [sx, sy, sz] = size;
    let px = sx + 2;
    let py = sy + 2;
    let pz = sz + 2;
    let total = checked_volume([px, py, pz])?;
    let plane = px * pz;
    let mut grid = vec![0_u8; total];

    for y in 0..sy {
        for z in 0..sz {
            let base = cell_index(0, y, z, sx, sz);
            for x in 0..sx {
                if cells[base + x] >= 0 {
                    grid[padded_index(x + 1, y + 1, z + 1, px, pz)] = 1;
                }
            }
        }
    }

    let mut queue = VecDeque::new();
    grid[0] = 2;
    queue.push_back(0_usize);

    while let Some(idx) = queue.pop_front() {
        let x = idx % px;
        let yz = idx / px;
        let z = yz % pz;
        let y = yz / pz;

        let mut visit = |n: usize| {
            if grid[n] == 0 {
                grid[n] = 2;
                queue.push_back(n);
            }
        };

        if x > 0 {
            visit(idx - 1);
        }
        if x + 1 < px {
            visit(idx + 1);
        }
        if z > 0 {
            visit(idx - px);
        }
        if z + 1 < pz {
            visit(idx + px);
        }
        if y > 0 {
            visit(idx - plane);
        }
        if y + 1 < py {
            visit(idx + plane);
        }
    }

    Ok(grid)
}

#[inline]
fn adjacent_to_exterior(local: [usize; 3], size: [usize; 3], exterior: &[u8]) -> bool {
    let [sx, _sy, sz] = size;
    let px = sx + 2;
    let pz = sz + 2;
    let plane = px * pz;
    let idx = padded_index(local[0] + 1, local[1] + 1, local[2] + 1, px, pz);
    [idx - 1, idx + 1, idx - px, idx + px, idx - plane, idx + plane]
        .into_iter()
        .any(|n| exterior[n] == 2)
}

#[inline]
fn neighbor_local(p: [usize; 3], d: [i32; 3], size: [usize; 3]) -> Option<[usize; 3]> {
    let nx = p[0] as i64 + d[0] as i64;
    let ny = p[1] as i64 + d[1] as i64;
    let nz = p[2] as i64 + d[2] as i64;
    if nx < 0 || ny < 0 || nz < 0 {
        return None;
    }
    let out = [nx as usize, ny as usize, nz as usize];
    if out[0] < size[0] && out[1] < size[1] && out[2] < size[2] {
        Some(out)
    } else {
        None
    }
}

fn all_ray_dirs() -> Vec<[i32; 3]> {
    let mut out = Vec::with_capacity(26);
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if dx != 0 || dy != 0 || dz != 0 {
                    out.push([dx, dy, dz]);
                }
            }
        }
    }
    out
}

fn update_entity_position(value: &mut Value, offset: [i32; 3]) {
    let Value::Compound(comp) = value else {
        return;
    };

    if let Some(Value::List(pos)) = comp.get_mut("pos") {
        if pos.len() >= 3 {
            for axis in 0..3 {
                match &mut pos[axis] {
                    Value::Double(v) => *v -= offset[axis] as f64,
                    Value::Float(v) => *v -= offset[axis] as f32,
                    Value::Int(v) => *v -= offset[axis],
                    Value::Long(v) => *v -= offset[axis] as i64,
                    _ => {}
                }
            }
        }
    }

    if let Some(Value::List(pos)) = comp.get_mut("blockPos") {
        if pos.len() >= 3 {
            for axis in 0..3 {
                match &mut pos[axis] {
                    Value::Int(v) => *v -= offset[axis],
                    Value::Long(v) => *v -= offset[axis] as i64,
                    _ => {}
                }
            }
        }
    }
}

fn parse_structure(bytes: &[u8]) -> Result<Structure> {
    fastnbt::from_bytes(bytes).map_err(|e| {
        anyhow!(
            "Este arquivo não parece ser um Structure NBT Vanilla/Create compatível: {e}"
        )
    })
}

pub fn process_file(
    input: PathBuf,
    output: Option<PathBuf>,
    options: ProcessOptions,
    tx: Sender<String>,
) -> Result<ProcessSummary> {
    send(&tx, format!("Lendo {}...", input.display()));
    let bytes = read_nbt_bytes(&input)?;
    let mut structure = parse_structure(&bytes)?;
    let original_size = validate_structure(&structure)?;
    let original_blocks = structure
        .blocks
        .iter()
        .filter(|b| !is_air(&structure.palette[b.state as usize].name))
        .count();

    send(
        &tx,
        format!(
            "Estrutura: {} × {} × {} | {:} blocos sólidos",
            original_size[0], original_size[1], original_size[2], original_blocks
        ),
    );

    let bounds = occupied_bounds(&structure)?;
    let size = bounds.size();
    send(
        &tx,
        format!(
            "Bounding box ocupado: {} × {} × {}. Preparando grade compacta...",
            size[0], size[1], size[2]
        ),
    );

    let (cells, solid_indices) = build_cells(&structure, bounds)?;
    let exterior = exterior_air_map(&cells, size)?;

    let palette_see_through: Vec<bool> = structure
        .palette
        .iter()
        .map(|p| is_see_through(&p.name, &options.extra_transparent_hints))
        .collect();
    let palette_falling: Vec<bool> = structure
        .palette
        .iter()
        .map(|p| is_falling(&p.name, &options.extra_falling_hints))
        .collect();

    let mut keep = vec![false; structure.blocks.len()];
    let mut visible_transparent = vec![false; structure.blocks.len()];
    let mut transparent_queue = VecDeque::new();

    send(&tx, "Detectando a casca externa...".to_string());
    for &block_idx in &solid_indices {
        let block = &structure.blocks[block_idx];
        let lp = local_pos(block, bounds);
        if adjacent_to_exterior(lp, size, &exterior) {
            keep[block_idx] = true;
            if palette_see_through[block.state as usize] {
                visible_transparent[block_idx] = true;
                transparent_queue.push_back(block_idx);
            }
        }
    }

    send(&tx, "Expandindo superfícies transparentes conectadas ao exterior...".to_string());
    while let Some(block_idx) = transparent_queue.pop_front() {
        let lp = local_pos(&structure.blocks[block_idx], bounds);
        for d in DIR6 {
            let Some(np) = neighbor_local(lp, d, size) else {
                continue;
            };
            let cell = cells[cell_index(np[0], np[1], np[2], size[0], size[2])];
            if cell < 0 {
                continue;
            }
            let ni = cell as usize;
            let state = structure.blocks[ni].state as usize;
            if palette_see_through[state] && !visible_transparent[ni] {
                visible_transparent[ni] = true;
                keep[ni] = true;
                transparent_queue.push_back(ni);
            }
        }
    }

    let transparent_external = visible_transparent.iter().filter(|x| **x).count();

    send(&tx, "Preservando blocos visíveis através de vidro e vazados...".to_string());
    let ray_dirs = all_ray_dirs();
    for (block_idx, visible) in visible_transparent.iter().enumerate() {
        if !*visible {
            continue;
        }
        let origin = local_pos(&structure.blocks[block_idx], bounds);

        // Vizinho imediato em todas as faces nunca é removido.
        for d in DIR6 {
            if let Some(np) = neighbor_local(origin, d, size) {
                let c = cells[cell_index(np[0], np[1], np[2], size[0], size[2])];
                if c >= 0 {
                    keep[c as usize] = true;
                }
            }
        }

        // Ray cast discreto em 26 direções.
        if options.transparent_depth > 0 {
            for d in &ray_dirs {
                for step in 1..=options.transparent_depth {
                    let nx = origin[0] as i64 + d[0] as i64 * step as i64;
                    let ny = origin[1] as i64 + d[1] as i64 * step as i64;
                    let nz = origin[2] as i64 + d[2] as i64 * step as i64;
                    if nx < 0
                        || ny < 0
                        || nz < 0
                        || nx >= size[0] as i64
                        || ny >= size[1] as i64
                        || nz >= size[2] as i64
                    {
                        break;
                    }
                    let c = cells[cell_index(
                        nx as usize,
                        ny as usize,
                        nz as usize,
                        size[0],
                        size[2],
                    )];
                    if c < 0 {
                        continue;
                    }
                    let ci = c as usize;
                    keep[ci] = true;
                    let state = structure.blocks[ci].state as usize;
                    if !palette_see_through[state] {
                        break;
                    }
                }
            }
        }
    }

    if options.preserve_block_entities {
        send(&tx, "Preservando Block Entities...".to_string());
        for &block_idx in &solid_indices {
            if structure.blocks[block_idx].extra.contains_key("nbt") {
                keep[block_idx] = true;
            }
        }
    }

    if options.thickness > 1 {
        send(
            &tx,
            format!("Aplicando espessura de {} blocos...", options.thickness),
        );
        let mut frontier: Vec<usize> = keep
            .iter()
            .enumerate()
            .filter_map(|(i, k)| k.then_some(i))
            .collect();
        for _ in 1..options.thickness {
            let mut next = Vec::new();
            for block_idx in frontier {
                let lp = local_pos(&structure.blocks[block_idx], bounds);
                for d in DIR6 {
                    let Some(np) = neighbor_local(lp, d, size) else {
                        continue;
                    };
                    let c = cells[cell_index(np[0], np[1], np[2], size[0], size[2])];
                    if c >= 0 {
                        let ci = c as usize;
                        if !keep[ci] {
                            keep[ci] = true;
                            next.push(ci);
                        }
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
    }

    let mut falling_supports_preserved = 0_usize;
    let mut unsupported_falling_blocks = 0_usize;
    if options.preserve_falling_supports {
        send(&tx, "Validando suporte de blocos com gravidade...".to_string());
        let mut q: VecDeque<usize> = solid_indices
            .iter()
            .copied()
            .filter(|i| keep[*i] && palette_falling[structure.blocks[*i].state as usize])
            .collect();
        let mut checked = vec![false; structure.blocks.len()];

        while let Some(block_idx) = q.pop_front() {
            if checked[block_idx] {
                continue;
            }
            checked[block_idx] = true;
            let lp = local_pos(&structure.blocks[block_idx], bounds);

            // Abaixo do bounding box não existe um bloco original para recuperar.
            if lp[1] == 0 {
                // Só conta como problema se a coordenada Y original não for a base do template.
                if structure.blocks[block_idx].pos[1] > 0 {
                    unsupported_falling_blocks += 1;
                }
                continue;
            }

            let below = [lp[0], lp[1] - 1, lp[2]];
            let c = cells[cell_index(below[0], below[1], below[2], size[0], size[2])];
            if c < 0 {
                unsupported_falling_blocks += 1;
                continue;
            }
            let support = c as usize;
            if !keep[support] {
                keep[support] = true;
                falling_supports_preserved += 1;
            }
            if palette_falling[structure.blocks[support].state as usize] {
                q.push_back(support);
            }
        }
    }

    send(&tx, "Removendo o miolo invisível...".to_string());
    let old_blocks = std::mem::take(&mut structure.blocks);
    structure.blocks = old_blocks
        .into_iter()
        .zip(keep)
        .filter_map(|(b, k)| {
            if k && !is_air(&structure.palette[b.state as usize].name) {
                Some(b)
            } else {
                None
            }
        })
        .collect();

    let kept_blocks = structure.blocks.len();
    let removed_blocks = original_blocks.saturating_sub(kept_blocks);
    let reduction_percent = if original_blocks == 0 {
        0.0
    } else {
        removed_blocks as f64 * 100.0 / original_blocks as f64
    };

    let mut final_size = original_size;
    if options.crop_empty_space && !structure.blocks.is_empty() {
        send(&tx, "Recortando espaço vazio das bordas...".to_string());
        let crop_bounds = occupied_bounds(&structure)?;
        let new_size = crop_bounds.size();
        let offset = crop_bounds.min;

        for block in &mut structure.blocks {
            for axis in 0..3 {
                block.pos[axis] -= offset[axis];
            }
        }
        for entity in &mut structure.entities {
            update_entity_position(entity, offset);
        }
        final_size = [new_size[0] as i32, new_size[1] as i32, new_size[2] as i32];
        structure.size = final_size.to_vec();
    }

    let output = output.unwrap_or_else(|| output_path_for(&input));
    if output == input {
        bail!("Por segurança, o arquivo de saída não pode ser igual ao arquivo original");
    }

    send(&tx, format!("Salvando {}...", output.display()));
    write_gzip_nbt(&output, &structure)?;

    send(
        &tx,
        format!(
            "Concluído: {:} blocos removidos ({:.2}%).",
            removed_blocks, reduction_percent
        ),
    );

    Ok(ProcessSummary {
        input,
        output,
        original_size,
        final_size,
        original_blocks,
        kept_blocks,
        removed_blocks,
        reduction_percent,
        transparent_external,
        falling_supports_preserved,
        unsupported_falling_blocks,
    })
}
