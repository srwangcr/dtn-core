# dtn-core — Motor DTN (BPv7) zero-allocation y no_std

## Índice / Index

- [Español](#español)
- [English](#english)
- [中文](#中文)
- [Deutsch](#deutsch)

---

## Español

`dtn-core` es una implementación de componentes centrales para un motor DTN (Bundle Protocol v7, RFC 9171) diseñada para entornos *bare-metal* y `no_std` de alto rendimiento. Prioriza:

- **Zero-Allocation y Zero-Copy** estricto en el hot-path.
- **Operaciones Lock-Free SPSC** alineadas a la caché para colas y buffers.
- **Garantías de Verificación Formal** (Miri compliant, libre de UB y data races).

### Contenido y Módulos Clave

- `src/lib.rs`: Punto de entrada del crate; exporta los módulos principales: `parser`, `storage`, `routing` y `telemetry`.

- **Parser CBOR y BPv7**
  - `src/parser/cbor.rs`: Helpers *zero-copy* para Canonical CBOR. Incluye `decode_unsigned()` para VARINT, `decode_definite_length()` y defensas contra malformed CBOR.
  - `src/parser/primary_block.rs`: Parser del Bloque Primario BPv7. Expone `ParsedBundleHeader<'a>`, `parse_primary_block()` con comprobación estricta de límites e inspección $O(1)$ vía `quick_validate_primary_block()`.

- **Almacenamiento y Buffers**
  - `src/storage/ring_buffer.rs`: `LockFreeRingBuffer<const SIZE: usize>` alineado a 64 bytes (`#[repr(align(64))]`) para prevenir *false sharing*. Utiliza semántica Acquire/Release para concurrencia SPSC sin asignaciones de memoria[cite: 2].
  - `src/storage/wal.rs`: Abstracción `DirectWal<const PAGES: usize>` alineada a páginas (`PAGE_SIZE = 4096`)[cite: 2]. Métodos `append()` y `flush_pages()` diseñados para Direct I/O (`O_DIRECT` / `pwrite`) mediante el trait `WalWriter`[cite: 2].

- **Enrutamiento**[cite: 2]
  - `src/routing/interval_tree.rs`: `CgrIntervalTree<const CAP: usize>`, un índice estático ordenado por `start_time` para búsquedas EAT (*Earliest Arrival Time*)[cite: 2]. Inserción/eliminación optimizada con `copy_within` (memmove vectorial) y búsqueda $O(\log N)$ con `find_next(t)`[cite: 2].

- **Telemetría**[cite: 2]
  - `src/telemetry/metrics.rs`: Métricas `no_std` basadas en `AtomicU64` para contadores y latencia acumulada (suma, min, max, count)[cite: 2]. Expone `snapshot()` y `reset()` atómicos[cite: 2].

---

### Diseño y Garantías

- **no_std:** Crate marcado con `#![no_std]`, totalmente libre de dependencias con `alloc` o el heap del sistema[cite: 2].
- **Zero-Copy:** Parsing de slices `&[u8]` devolviendo referencias con lifetimes en cero-copia[cite: 2].
- **Lock-Free SPSC:** `LockFreeRingBuffer` utiliza `AtomicUsize` con acoplamiento SPSC de alto rendimiento[cite: 2].
- **Alineación de Memoria:** Cache-line padding (64 bytes) para mitigar el *false sharing* en CPU y alineación a 4096 bytes para Direct I/O[cite: 2].

---

### Guía de Pruebas y Benchmarking

#### 1. Pruebas Unitarias y Miri

```fish
# Tests unitarios estándar
cargo test --lib

# Verificación de Pointer Provenance y Data Races con Miri
cargo +nightly miri test --lib
```

---

## English

`dtn-core` is an implementation of core components for a DTN engine (Bundle Protocol v7, RFC 9171) designed for *bare-metal* and `no_std` high-performance environments. It prioritizes:

- **Strict Zero-Allocation and Zero-Copy** in the hot-path.
- **Cache-aligned Lock-Free SPSC** operations for queues and buffers.
- **Formal Verification Guarantees** (Miri compliant, free of UB and data races).

### Contents and Key Modules

- `src/lib.rs`: Crate entry point; exports the main modules: `parser`, `storage`, `routing`, and `telemetry`.

- **CBOR and BPv7 Parser**
  - `src/parser/cbor.rs`: *Zero-copy* helpers for Canonical CBOR. Includes `decode_unsigned()` for VARINT, `decode_definite_length()`, and defenses against malformed CBOR.
  - `src/parser/primary_block.rs`: BPv7 Primary Block parser. Exposes `ParsedBundleHeader<'a>`, `parse_primary_block()` with strict bounds checking, and $O(1)$ inspection via `quick_validate_primary_block()`.

- **Storage and Buffers**
  - `src/storage/ring_buffer.rs`: `LockFreeRingBuffer<const SIZE: usize>` aligned to 64 bytes (`#[repr(align(64))]`) to prevent false sharing. Uses Acquire/Release semantics for SPSC concurrency without memory allocations[cite: 2].
  - `src/storage/wal.rs`: `DirectWal<const PAGES: usize>` abstraction aligned to pages (`PAGE_SIZE = 4096`)[cite: 2]. `append()` and `flush_pages()` methods designed for Direct I/O (`O_DIRECT` / `pwrite`) via the `WalWriter` trait[cite: 2].

- **Routing**[cite: 2]
  - `src/routing/interval_tree.rs`: `CgrIntervalTree<const CAP: usize>`, a static index sorted by `start_time` for EAT (*Earliest Arrival Time*) lookups[cite: 2]. Optimized insertion/deletion with `copy_within` (vectorized memmove) and $O(\log N)$ search with `find_next(t)`[cite: 2].

- **Telemetry**[cite: 2]
  - `src/telemetry/metrics.rs`: `no_std` metrics based on `AtomicU64` for counters and accumulated latency (sum, min, max, count)[cite: 2]. Exposes atomic `snapshot()` and `reset()`[cite: 2].

---

### Design and Guarantees

- **no_std:** Crate marked with `#![no_std]`, completely free of dependencies on `alloc` or the system heap[cite: 2].
- **Zero-Copy:** Parsing `&[u8]` slices returning zero-copy references with lifetimes[cite: 2].
- **Lock-Free SPSC:** `LockFreeRingBuffer` uses `AtomicUsize` with high-performance SPSC coupling[cite: 2].
- **Memory Alignment:** Cache-line padding (64 bytes) to mitigate false sharing on CPUs and 4096-byte alignment for Direct I/O[cite: 2].

---

### Testing and Benchmarking Guide

#### 1. Unit Tests and Miri

```fish
# Standard unit tests
cargo test --lib

# Pointer Provenance and Data Race verification with Miri
cargo +nightly miri test --lib
```

---

## 中文

`dtn-core` 是一个为高性能*裸机*和 `no_std` 环境设计的 DTN 引擎（Bundle Protocol v7，RFC 9171）核心组件实现。其优先考虑：

- **热路径中的严格零分配和零拷贝**。
- **缓存行对齐的无锁 SPSC** 队列和缓冲区操作。
- **形式验证保证**（兼容 Miri，无未定义行为和数据竞争）。

### 内容与关键模块

- `src/lib.rs`：crate 入口点；导出主要模块：`parser`、`storage`、`routing` 和 `telemetry`。

- **CBOR 和 BPv7 解析器**
  - `src/parser/cbor.rs`：规范 CBOR 的*零拷贝*辅助函数。包括用于 VARINT 的 `decode_unsigned()`、`decode_definite_length()` 以及针对格式错误 CBOR 的防御。
  - `src/parser/primary_block.rs`：BPv7 主块解析器。暴露 `ParsedBundleHeader<'a>`、带有严格边界检查的 `parse_primary_block()`，以及通过 `quick_validate_primary_block()` 实现的 $O(1)$ 检查。

- **存储与缓冲区**
  - `src/storage/ring_buffer.rs`：`LockFreeRingBuffer<const SIZE: usize>`，对齐到 64 字节（`#[repr(align(64))]`）以防止伪共享。使用 Acquire/Release 语义实现 SPSC 并发，无需内存分配[cite: 2]。
  - `src/storage/wal.rs`：`DirectWal<const PAGES: usize>` 抽象，对齐到页（`PAGE_SIZE = 4096`）[cite: 2]。`append()` 和 `flush_pages()` 方法通过 `WalWriter` trait 为 Direct I/O（`O_DIRECT` / `pwrite`）设计[cite: 2]。

- **路由**[cite: 2]
  - `src/routing/interval_tree.rs`：`CgrIntervalTree<const CAP: usize>`，一个按 `start_time` 排序的静态索引，用于 EAT（*最早到达时间*）查找[cite: 2]。使用 `copy_within`（向量化 memmove）优化插入/删除，并通过 `find_next(t)` 实现 $O(\log N)$ 搜索[cite: 2]。

- **遥测**[cite: 2]
  - `src/telemetry/metrics.rs`：基于 `AtomicU64` 的 `no_std` 指标，用于计数器和累积延迟（总和、最小值、最大值、计数）[cite: 2]。暴露原子操作 `snapshot()` 和 `reset()`[cite: 2]。

---

### 设计与保证

- **no_std:** crate 标记为 `#![no_std]`，完全无 `alloc` 或系统堆的依赖[cite: 2]。
- **零拷贝:** 解析 `&[u8]` 切片，返回带生命周期的零拷贝引用[cite: 2]。
- **无锁 SPSC:** `LockFreeRingBuffer` 使用 `AtomicUsize` 实现高性能 SPSC 耦合[cite: 2]。
- **内存对齐:** 缓存行填充（64 字节）以减轻 CPU 上的伪共享，以及 4096 字节对齐用于 Direct I/O[cite: 2]。

---

### 测试与基准测试指南

#### 1. 单元测试与 Miri

```fish
# 标准单元测试
cargo test --lib

# 使用 Miri 进行指针来源和数据竞争验证
cargo +nightly miri test --lib
```

---

## Deutsch

`dtn-core` ist eine Implementierung von Kernkomponenten für eine DTN-Engine (Bundle Protocol v7, RFC 9171), die für *Bare-Metal*- und `no_std`-Hochleistungsumgebungen entwickelt wurde. Schwerpunkte sind:

- **Strikte Zero-Allocation und Zero-Copy** im Hot-Path.
- **Cache-alignte Lock-Free SPSC**-Operationen für Warteschlangen und Puffer.
- **Formale Verifikationsgarantien** (Miri-konform, frei von UB und Datenrennen).

### Inhalt und Schlüsselmodule

- `src/lib.rs`: Einstiegspunkt der Crate; exportiert die Hauptmodule: `parser`, `storage`, `routing` und `telemetry`.

- **CBOR- und BPv7-Parser**
  - `src/parser/cbor.rs`: *Zero-Copy*-Hilfsfunktionen für Canonical CBOR. Enthält `decode_unsigned()` für VARINT, `decode_definite_length()` sowie Abwehrmaßnahmen gegen fehlerhaftes CBOR.
  - `src/parser/primary_block.rs`: Parser für den BPv7-Primärblock. Stellt `ParsedBundleHeader<'a>`, `parse_primary_block()` mit strenger Grenzprüfung und $O(1)$-Inspektion über `quick_validate_primary_block()` bereit.

- **Speicher und Puffer**
  - `src/storage/ring_buffer.rs`: `LockFreeRingBuffer<const SIZE: usize>` ausgerichtet auf 64 Bytes (`#[repr(align(64))]`) zur Vermeidung von *False Sharing*. Verwendet Acquire/Release-Semantik für SPSC-Konkurrenz ohne Speicherzuweisungen[cite: 2].
  - `src/storage/wal.rs`: `DirectWal<const PAGES: usize>`-Abstraktion, ausgerichtet auf Seitengrößen (`PAGE_SIZE = 4096`)[cite: 2]. Die Methoden `append()` und `flush_pages()` sind für Direct I/O (`O_DIRECT` / `pwrite`) über das `WalWriter`-Trait ausgelegt[cite: 2].

- **Routing**[cite: 2]
  - `src/routing/interval_tree.rs`: `CgrIntervalTree<const CAP: usize>`, ein statischer Index sortiert nach `start_time` für EAT (*Earliest Arrival Time*)-Suchanfragen[cite: 2]. Optimierte Einfügung/Löschung mit `copy_within` (vektorisiertes memmove) und $O(\log N)$-Suche mit `find_next(t)`[cite: 2].

- **Telemetrie**[cite: 2]
  - `src/telemetry/metrics.rs`: `no_std`-Metriken basierend auf `AtomicU64` für Zähler und kumulierte Latenz (Summe, Minimum, Maximum, Anzahl)[cite: 2]. Bietet atomare `snapshot()`- und `reset()`-Methoden[cite: 2].

---

### Design und Garantien

- **no_std:** Crate gekennzeichnet mit `#![no_std]`, vollständig frei von Abhängigkeiten zu `alloc` oder dem System-Heap[cite: 2].
- **Zero-Copy:** Parsing von `&[u8]`-Slices, die Zero-Copy-Referenzen mit Lifetimes zurückgeben[cite: 2].
- **Lock-Free SPSC:** `LockFreeRingBuffer` verwendet `AtomicUsize` mit hochleistungsfähiger SPSC-Kopplung[cite: 2].
- **Speicherausrichtung:** Cache-Zeilen-Auffüllung (64 Bytes) zur Minderung von *False Sharing* auf CPUs sowie 4096-Byte-Ausrichtung für Direct I/O[cite: 2].

---

### Test- und Benchmarking-Anleitung

#### 1. Unit-Tests und Miri

```fish
# Standard-Unit-Tests
cargo test --lib

# Überprüfung von Pointer Provenance und Datenrennen mit Miri
cargo +nightly miri test --lib
```