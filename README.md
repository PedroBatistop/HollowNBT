# Hollow NBT

Aplicativo Windows em Rust para transformar estruturas Minecraft Vanilla/Create `.nbt` em uma casca externa.

## Recursos

- Detecta somente ar conectado ao exterior, evitando preservar cavernas internas fechadas.
- Mantém blocos visíveis através de vidro/blocos vazados.
- Espessura configurável da casca.
- Mantém suporte sob blocos com gravidade.
- Preserva Block Entities opcionalmente.
- Recorta automaticamente espaço vazio do bounding box.
- Reposiciona blocos e entidades após o crop.
- Lê NBT GZip ou sem compressão e grava NBT GZip.
- Interface gráfica e drag-and-drop.

## Build local

```powershell
cargo build --release
```

O executável será criado em:

```text
target\release\hollow-nbt.exe
```

## GitHub Actions

O workflow `.github/workflows/build-windows.yml` gera automaticamente `HollowNBT.exe` em um runner Windows e publica o binário como artifact da execução.
