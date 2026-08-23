# dtn-core — Motor DTN (BPv7) zero-allocation y no_std

## Índice / Index / Inhaltsverzeichnis / 目录

- [Español](#español)
- [English](#english)
- [Deutsch](#deutsch)
- [中文](#中文)

---

## Español

`dtn-core` es una implementación de componentes centrales para un motor DTN (Bundle Protocol v7, RFC 9171) diseñada para entornos *bare-metal* y `no_std` de alto rendimiento. Prioriza:

- **Zero-Allocation y Zero-Copy** cuando es posible.
- **Operaciones lock-free** de alto rendimiento (SPSC) para colas y buffers.
- **Componentes simples**, fáciles de auditar y probar.

### Contenido y módulos clave

- `src/lib.rs`: Punto de entrada del crate; exporta los módulos principales: `parser`, `storage`, `routing` y `telemetry`.

- **Parser CBOR y BPv7**
  - [src/parser/cbor.rs](src/parser/cbor.rs#L1-L200): Helpers *zero-copy* para Canonical CBOR. Incluye `decode_unsigned()` para enteros CBOR (VARINT), `decode_definite_length()` para longitudes y `major_type()`.
  - [src/parser/primary_block.rs](src/parser/primary_block.rs#L1-L300): Parser *zero-copy* del Bloque Primario BPv7. Expone `ParsedBundleHeader<'a>`, `parse_primary_block()` y `quick_validate_primary_block()`.
  - [src/parser/mod.rs](src/parser/mod.rs#L1-L20): Exporta los submódulos del parser.

- **Almacenamiento y buffers**
  - [src/storage/ring_buffer.rs](src/storage/ring_buffer.rs#L1-L300): `LockFreeRingBuffer<const SIZE: usize>` alineado a 64 bytes. Implementa `push`, `pop` y los helpers `push_byte`/`pop_byte`. Diseñado para SPSC sin asignaciones de memoria[cite: 2].
  - [src/storage/wal.rs](src/storage/wal.rs#L1-L400): Abstracción `DirectWal<const PAGES: usize>` con buffer alineado a páginas (`PAGE_SIZE = 4096`) y el trait `WalWriter`[cite: 2]. Incluye los métodos `append()` y `flush_pages()` para preparar páginas para Direct I/O[cite: 2].

- **Enrutamiento**[cite: 2]
  - [src/routing/interval_tree.rs](src/routing/interval_tree.rs#L1-L400): `CgrIntervalTree<const CAP: usize>`, un índice estático ordenado por `start_time` para búsquedas EAT (*Earliest Arrival Time*)[cite: 2]. Inserción ordenada y búsqueda $O(\log N)$ mediante `find_next(t)`[cite: 2].

- **Telemetría**[cite: 2]
  - [src/telemetry/metrics.rs](src/telemetry/metrics.rs#L1-L300): Módulo de métricas `no_std` basado en `AtomicU64` para contadores y latencia (suma, mín, máx, recuento)[cite: 2]. Expone `snapshot()` y `reset()`[cite: 2].

### Diseño y garantías

- **no_std:** El crate está marcado con `#![no_std]` y evita llamadas al heap; las pruebas ejecutan `cargo test` durante desarrollo, pero el código está diseñado para compilar sin `std` en entornos embebidos[cite: 2].
- **Zero-Copy:** Los parsers aceptan `&[u8]` y devuelven referencias con lifetimes (`&str`, `&[u8]`) apuntando al buffer de entrada — no se realizan `alloc` ni copias innecesarias[cite: 2].
- **Lock-Free SPSC:** `LockFreeRingBuffer` utiliza exclusivamente `AtomicUsize` con `Ordering::Acquire`/`Ordering::Release`[cite: 2]. Asume un modelo *Single-Producer Single-Consumer* para maximizar el rendimiento y prescinde de `Mutex`[cite: 2].
- **Alineación de memoria:** Los buffers críticos están alineados a líneas de caché o páginas (`#[repr(align(64))]` y `#[repr(align(4096))]`) para mitigar el *false sharing* y facilitar Direct I/O[cite: 2].
- **WAL por páginas:** `DirectWal` prepara páginas completas para escritura mediante Direct I/O[cite: 2]. Está diseñado para integrarse con sinks que implementen `WalWriter` y utilicen `O_DIRECT`/`pwrite` específicos del sistema operativo (la implementación del sink queda fuera del crate core para mantener el entorno `no_std`)[cite: 2].

### APIs principales (resumen rápido)

- **Parser de Primary Block**[cite: 2]
  - `parse_primary_block(buf: &[u8]) -> Result<ParsedBundleHeader<'_>, &'static str>` — Parsea el bloque primario CBOR sin realizar asignaciones[cite: 2].
  - `quick_validate_primary_block(buf: &[u8]) -> bool` — Validación rápida $O(1)$ del encabezado[cite: 2].

- **Ring Buffer (SPSC)**[cite: 2]
  - `let rb: LockFreeRingBuffer<N> = LockFreeRingBuffer::new();`[cite: 2]
  - `rb.push(&data)` — Intenta insertar bytes; devuelve `Err(available_space)` si no hay espacio disponible[cite: 2].
  - `rb.pop(&mut dst)` — Extrae hasta `dst.len()` bytes y devuelve la cantidad leída[cite: 2].

- **WAL (page-aligned)**[cite: 2]
  - `let mut wal: DirectWal<PAGES> = DirectWal::new();`[cite: 2]
  - `wal.append(&bytes)` — Copia bytes al buffer de páginas sin realizar asignaciones[cite: 2].
  - `wal.flush_pages(&mut writer)` — Ejecuta `writer.write_page(&[u8; 4096])` por cada página completa[cite: 2].

- **Interval Tree (EAT)**[cite: 2]
  - `let mut tree: CgrIntervalTree<CAP> = CgrIntervalTree::new();`[cite: 2]
  - `tree.insert(contact)` — Inserta y mantiene el orden por `start_time`[cite: 2].
  - `tree.find_next(t)` — Búsqueda $O(\log N)$ para la ventana que contiene a `t` o la próxima disponible[cite: 2].

- **Telemetría**[cite: 2]
  - `let m = Metrics::new(); m.incr_processed(1); m.record_latency_ns(123); let s = m.snapshot();`[cite: 2]

### Compilación y pruebas

Durante el desarrollo en un host con `std` disponible, podés ejecutar[cite: 2]:

```bash
cargo test --lib