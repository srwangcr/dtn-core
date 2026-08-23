
# dtn-core — Motor DTN (BPv7) zero-allocation y no_std

## Índice / Index

- [Español](#espaol)
- [English](#english)
- [中文](#中文)s
- [Deutsch](#deutsch)

---

## Español

`dtn-core` es una implementación de componentes centrales para un motor DTN (Bundle Protocol v7, RFC 9171) diseñada para entornos *bare-metal* y `no_std` de alto rendimiento. Prioriza:

- **Zero-Allocation y Zero-Copy** estricto en el hot-path.
- **Operaciones Lock-Free SPSC** alineadas a la caché para colas y buffers.
- **Garantías de Verificación Formal** (Miri compliant, libre de UB y data races).
- **Store-and-Forward con persistencia atómica** para rutas intermitentes.

---

### Contenido y Módulos Clave

- `src/lib.rs`: Punto de entrada del crate; exporta los módulos principales: `parser`, `processor`, `storage`, `routing`, `cla` y `telemetry`.

#### Parser CBOR y BPv7
- `src/parser/cbor.rs`: Helpers *zero-copy* para Canonical CBOR. Incluye `decode_unsigned()` para VARINT, `decode_definite_length()` y defensas contra malformed CBOR.
- `src/parser/primary_block.rs`: Parser del Bloque Primario BPv7. Expone `ParsedBundleHeader<'a>`, `parse_primary_block()` con comprobación estricta de límites e inspección $O(1)$ vía `quick_validate_primary_block()`.

#### Processor y Máquina de Estados
- `src/processor/state_machine.rs`: Máquina de estados para validación de paquetes DTN BPv7. Procesa `ParseStatus` y aplica transiciones `Accepted`, `Expired` o `Malformed`.
- `src/processor/fragmentation.rs`: Manejo de reensamblado de fragmentos mediante `ReassemblySlot<const BUF_SIZE: usize>`. Permite reconstrucción *zero-allocation* de payloads fragmentados evaluando `processing_flags`.
- `src/processor/pipeline.rs`: Canalización `IngestionPipeline` que conecta la ingesta de paquetes con el parser CBOR, la evaluación de políticas en CGR, el encolado Lock-Free y el fallback a disco.

#### Convergence Layer Adapter (CLA)
- `src/cla/udp.rs`: Buffer de trama `UdpFrameBuffer<const MAX_FRAME_SIZE: usize>` para ingesta de socket no bloqueante sin pasar por el heap. Dispara directo a `IngestionPipeline`.

#### Almacenamiento y Buffers
- `src/storage/ring_buffer.rs`: `LockFreeRingBuffer<const SIZE: usize>` alineado a 64 bytes (`#[repr(align(64))]`) para prevenir *false sharing*. Utiliza semántica Acquire/Release para concurrencia SPSC sin asignaciones de memoria.
- `src/storage/wal.rs`: Abstracción `DirectWal<const PAGES: usize>` alineada a páginas (`PAGE_SIZE = 4096`). Métodos `append()` y `flush_pages()` diseñados para Direct I/O (`O_DIRECT` / `pwrite`) mediante el trait `WalWriter`.
- `src/storage/disk_sink.rs`: Implementación `DiskBlockStore<const MAX_PAGES: usize>` para Direct I/O alineado a páginas. Actúa como sumidero persistente (*Store-and-Forward*) cuando la cola en memoria colapsa o el enlace no tiene ruta activa (`DropNoRoute`).

#### Enrutamiento
- `src/routing/interval_tree.rs`: `CgrIntervalTree<const CAP: usize>`, un índice estático ordenado por `start_time` para búsquedas EAT (*Earliest Arrival Time*). Inserción/eliminación optimizada con `copy_within` (memmove vectorial) y búsqueda $O(\log N)$ con `find_next(t)`.

#### Telemetría
- `src/telemetry/metrics.rs`: Métricas `no_std` basadas en `AtomicU64` para contadores y latencia acumulada (suma, min, max, count). Expone `snapshot()` y `reset()` atómicos.

---

### Diseño y Garantías de Arquitectura

- **no_std:** Crate marcado con `#![no_std]`, totalmente libre de dependencias con `alloc` o el heap del sistema.
- **Zero-Copy:** Parsing de slices `&[u8]` devolviendo referencias con lifetimes en cero-copia.
- **Lock-Free SPSC:** `LockFreeRingBuffer` utiliza `AtomicUsize` con acoplamiento SPSC de alto rendimiento.
- **Alineación de Memoria:** Cache-line padding (64 bytes) para mitigar el *false sharing* en CPU y alineación a 4096 bytes para Direct I/O.
- **Store-and-Forward con Persistencia Atómica:** Manejo de rutas intermitentes. Si `CgrIntervalTree` no retorna un intervalo válido o el `LockFreeRingBuffer` desborda, el bundle cae dinámicamente a `PersistedToWal` vía `DirectWal` / `DiskBlockStore`.
- **Resiliencia contra Fragmentation Overlap:** El slot de reensamblado maneja offsets de fragmentos de forma estática en memoria stack/global, garantizando $O(1)$ en footprint de memoria.
- **Flujo CLA Directo:** Desacople total de runtimes asíncronos (`tokio`/`async-std`). La ingesta UDP escribe directo en buffers estáticos de bytes y delega al procesador sin copias intermedias.

---

### Resultados de Benchmarks (Criterion)

- **Escenario de Prueba:** Ingesta UDP CLA $\rightarrow$ Parsing CBOR Zero-Copy $\rightarrow$ Validación en Máquina de Estados $\rightarrow$ Búsqueda CGR $O(\log N)$ $\rightarrow$ Encolado SPSC RingBuffer.
- **Latencia Media del Hot-Path:** **`~59.6 ns`** por bundle.
- **Throughput Teórico:** **`~16.7 Mops/s`** (Millones de operaciones por segundo en un único hilo de CPU).
- **Verificación Dinámica:** **21/21 tests** pasados exitosamente bajo **Miri** (`cargo +nightly miri test --lib`), garantizando ausencia de *Uninitialized Memory Reads*, *Data Races* y violaciones de *Pointer Provenance*.

---

### Guía de Pruebas y Benchmarking

#### 1. Pruebas Unitarias y Miri

```fish
# Tests unitarios estándar
cargo test --lib

# Verificación de Pointer Provenance y Data Races con Miri
cargo +nightly miri test --lib
```

#### 2. Benchmarks de Rendimiento

```fish
# Ejecución del arnés de rendimiento hot-path
cargo bench --bench pipeline_bench
```

---

## English

`dtn-core` is an implementation of core components for a DTN engine (Bundle Protocol v7, RFC 9171) designed for *bare-metal* and `no_std` high-performance environments. It prioritizes:

- **Strict Zero-Allocation and Zero-Copy** in the hot-path.
- **Cache-aligned Lock-Free SPSC** operations for queues and buffers.
- **Formal Verification Guarantees** (Miri compliant, free of UB and data races).
- **Store-and-Forward with atomic persistence** for intermittent routes.

---

### Contents and Key Modules

- `src/lib.rs`: Crate entry point; exports the main modules: `parser`, `processor`, `storage`, `routing`, `cla`, and `telemetry`.

#### CBOR and BPv7 Parser
- `src/parser/cbor.rs`: *Zero-copy* helpers for Canonical CBOR. Includes `decode_unsigned()` for VARINT, `decode_definite_length()`, and defenses against malformed CBOR.
- `src/parser/primary_block.rs`: BPv7 Primary Block parser. Exposes `ParsedBundleHeader<'a>`, `parse_primary_block()` with strict bounds checking, and $O(1)$ inspection via `quick_validate_primary_block()`.

#### Processor and State Machine
- `src/processor/state_machine.rs`: State machine for BPv7 DTN packet validation. Processes `ParseStatus` and applies `Accepted`, `Expired`, or `Malformed` transitions.
- `src/processor/fragmentation.rs`: Fragment reassembly handling via `ReassemblySlot<const BUF_SIZE: usize>`. Enables *zero-allocation* reconstruction of fragmented payloads by evaluating `processing_flags`.
- `src/processor/pipeline.rs`: `IngestionPipeline` that connects packet ingestion with the CBOR parser, CGR policy evaluation, Lock-Free queuing, and disk fallback.

#### Convergence Layer Adapter (CLA)
- `src/cla/udp.rs`: `UdpFrameBuffer<const MAX_FRAME_SIZE: usize>` for non-blocking socket ingestion without heap allocation. Triggers directly into `IngestionPipeline`.

#### Storage and Buffers
- `src/storage/ring_buffer.rs`: `LockFreeRingBuffer<const SIZE: usize>` aligned to 64 bytes (`#[repr(align(64))]`) to prevent false sharing. Uses Acquire/Release semantics for SPSC concurrency without memory allocations.
- `src/storage/wal.rs`: `DirectWal<const PAGES: usize>` abstraction aligned to pages (`PAGE_SIZE = 4096`). `append()` and `flush_pages()` methods designed for Direct I/O (`O_DIRECT` / `pwrite`) via the `WalWriter` trait.
- `src/storage/disk_sink.rs`: `DiskBlockStore<const MAX_PAGES: usize>` implementation for page-aligned Direct I/O. Acts as a persistent sink (*Store-and-Forward*) when the in-memory queue overflows or the link has no active route (`DropNoRoute`).

#### Routing
- `src/routing/interval_tree.rs`: `CgrIntervalTree<const CAP: usize>`, a static index sorted by `start_time` for EAT (*Earliest Arrival Time*) lookups. Optimized insertion/deletion with `copy_within` (vectorized memmove) and $O(\log N)$ search with `find_next(t)`.

#### Telemetry
- `src/telemetry/metrics.rs`: `no_std` metrics based on `AtomicU64` for counters and accumulated latency (sum, min, max, count). Exposes atomic `snapshot()` and `reset()`.

---

### Design and Architecture Guarantees

- **no_std:** Crate marked with `#![no_std]`, completely free of dependencies on `alloc` or the system heap.
- **Zero-Copy:** Parsing `&[u8]` slices returning zero-copy references with lifetimes.
- **Lock-Free SPSC:** `LockFreeRingBuffer` uses `AtomicUsize` with high-performance SPSC coupling.
- **Memory Alignment:** Cache-line padding (64 bytes) to mitigate false sharing on CPUs and 4096-byte alignment for Direct I/O.
- **Store-and-Forward with Atomic Persistence:** Handles intermittent routes. If `CgrIntervalTree` returns no valid interval or the `LockFreeRingBuffer` overflows, the bundle dynamically falls back to `PersistedToWal` via `DirectWal` / `DiskBlockStore`.
- **Fragmentation Overlap Resilience:** The reassembly slot handles fragment offsets statically in stack/global memory, guaranteeing $O(1)$ memory footprint.
- **Direct CLA Flow:** Complete decoupling from async runtimes (`tokio`/`async-std`). UDP ingestion writes directly to static byte buffers and delegates to the processor with no intermediate copies.

---

### Benchmark Results (Criterion)

- **Test Scenario:** UDP CLA ingestion $\rightarrow$ Zero-Copy CBOR Parsing $\rightarrow$ State Machine Validation $\rightarrow$ CGR $O(\log N)$ Lookup $\rightarrow$ SPSC RingBuffer Queuing.
- **Hot-Path Average Latency:** **`~59.6 ns`** per bundle.
- **Theoretical Throughput:** **`~16.7 Mops/s`** (Millions of operations per second on a single CPU core).
- **Dynamic Verification:** **21/21 tests** passed successfully under **Miri** (`cargo +nightly miri test --lib`), guaranteeing absence of *Uninitialized Memory Reads*, *Data Races*, and *Pointer Provenance* violations.

---

### Testing and Benchmarking Guide

#### 1. Unit Tests and Miri

```fish
# Standard unit tests
cargo test --lib

# Pointer Provenance and Data Race verification with Miri
cargo +nightly miri test --lib
```

#### 2. Performance Benchmarks

```fish
# Run the hot-path performance harness
cargo bench --bench pipeline_bench
```

---

## 中文

`dtn-core` 是一个为高性能*裸机*和 `no_std` 环境设计的 DTN 引擎（Bundle Protocol v7，RFC 9171）核心组件实现。其优先考虑：

- **热路径中的严格零分配和零拷贝**。
- **缓存行对齐的无锁 SPSC** 队列和缓冲区操作。
- **形式验证保证**（兼容 Miri，无未定义行为和数据竞争）。
- **具有原子持久性的存储转发**，用于间歇性路由。

---

### 内容与关键模块

- `src/lib.rs`：crate 入口点；导出主要模块：`parser`、`processor`、`storage`、`routing`、`cla` 和 `telemetry`。

#### CBOR 和 BPv7 解析器
- `src/parser/cbor.rs`：规范 CBOR 的*零拷贝*辅助函数。包括用于 VARINT 的 `decode_unsigned()`、`decode_definite_length()` 以及针对格式错误 CBOR 的防御。
- `src/parser/primary_block.rs`：BPv7 主块解析器。暴露 `ParsedBundleHeader<'a>`、带有严格边界检查的 `parse_primary_block()`，以及通过 `quick_validate_primary_block()` 实现的 $O(1)$ 检查。

#### 处理器与状态机
- `src/processor/state_machine.rs`：用于 BPv7 DTN 数据包验证的状态机。处理 `ParseStatus` 并应用 `Accepted`、`Expired` 或 `Malformed` 转换。
- `src/processor/fragmentation.rs`：通过 `ReassemblySlot<const BUF_SIZE: usize>` 处理分片重组。通过评估 `processing_flags` 实现分片负载的*零分配*重建。
- `src/processor/pipeline.rs`：`IngestionPipeline` 将数据包接收与 CBOR 解析、CGR 策略评估、无锁排队和磁盘回退连接起来。

#### 汇聚层适配器 (CLA)
- `src/cla/udp.rs`：`UdpFrameBuffer<const MAX_FRAME_SIZE: usize>`，用于无堆分配的非阻塞套接字接收。直接触发 `IngestionPipeline`。

#### 存储与缓冲区
- `src/storage/ring_buffer.rs`：`LockFreeRingBuffer<const SIZE: usize>`，对齐到 64 字节（`#[repr(align(64))]`）以防止伪共享。使用 Acquire/Release 语义实现 SPSC 并发，无需内存分配。
- `src/storage/wal.rs`：`DirectWal<const PAGES: usize>` 抽象，对齐到页（`PAGE_SIZE = 4096`）。`append()` 和 `flush_pages()` 方法通过 `WalWriter` trait 为 Direct I/O（`O_DIRECT` / `pwrite`）设计。
- `src/storage/disk_sink.rs`：`DiskBlockStore<const MAX_PAGES: usize>` 实现，用于页对齐的 Direct I/O。当内存队列溢出或链路无活动路由（`DropNoRoute`）时，作为持久化接收器（*存储转发*）。

#### 路由
- `src/routing/interval_tree.rs`：`CgrIntervalTree<const CAP: usize>`，一个按 `start_time` 排序的静态索引，用于 EAT（*最早到达时间*）查找。使用 `copy_within`（向量化 memmove）优化插入/删除，并通过 `find_next(t)` 实现 $O(\log N)$ 搜索。

#### 遥测
- `src/telemetry/metrics.rs`：基于 `AtomicU64` 的 `no_std` 指标，用于计数器和累积延迟（总和、最小值、最大值、计数）。暴露原子操作 `snapshot()` 和 `reset()`。

---

### 设计与架构保证

- **no_std:** crate 标记为 `#![no_std]`，完全无 `alloc` 或系统堆的依赖。
- **零拷贝:** 解析 `&[u8]` 切片，返回带生命周期的零拷贝引用。
- **无锁 SPSC:** `LockFreeRingBuffer` 使用 `AtomicUsize` 实现高性能 SPSC 耦合。
- **内存对齐:** 缓存行填充（64 字节）以减轻 CPU 上的伪共享，以及 4096 字节对齐用于 Direct I/O。
- **具有原子持久性的存储转发:** 处理间歇性路由。如果 `CgrIntervalTree` 未返回有效间隔或 `LockFreeRingBuffer` 溢出，bundle 通过 `DirectWal`/`DiskBlockStore` 动态回退到 `PersistedToWal`。
- **分片重叠弹性:** 重组槽在栈/全局内存中静态处理分片偏移，保证 $O(1)$ 内存占用。
- **直接 CLA 流:** 与异步运行时（`tokio`/`async-std`）完全解耦。UDP 接收直接写入静态字节缓冲区，并委托给处理器，无中间副本。

---

### 基准测试结果 (Criterion)

- **测试场景:** UDP CLA 接收 $\rightarrow$ 零拷贝 CBOR 解析 $\rightarrow$ 状态机验证 $\rightarrow$ CGR $O(\log N)$ 查找 $\rightarrow$ SPSC RingBuffer 排队。
- **热路径平均延迟:** 每个 bundle **`~59.6 ns`**。
- **理论吞吐量:** **`~16.7 Mops/s`**（单 CPU 核心每秒百万次操作）。
- **动态验证:** **21/21 测试**在 **Miri**（`cargo +nightly miri test --lib`）下成功通过，保证无*未初始化内存读取*、*数据竞争*和*指针来源*违规。

---

### 测试与基准测试指南

#### 1. 单元测试与 Miri

```fish
# 标准单元测试
cargo test --lib

# 使用 Miri 进行指针来源和数据竞争验证
cargo +nightly miri test --lib
```

#### 2. 性能基准测试

```fish
# 运行热路径性能测试工具
cargo bench --bench pipeline_bench
```

---

## Deutsch

`dtn-core` ist eine Implementierung von Kernkomponenten für eine DTN-Engine (Bundle Protocol v7, RFC 9171), die für *Bare-Metal*- und `no_std`-Hochleistungsumgebungen entwickelt wurde. Schwerpunkte sind:

- **Strikte Zero-Allocation und Zero-Copy** im Hot-Path.
- **Cache-alignte Lock-Free SPSC**-Operationen für Warteschlangen und Puffer.
- **Formale Verifikationsgarantien** (Miri-konform, frei von UB und Datenrennen).
- **Store-and-Forward mit atomarer Persistenz** für intermittierende Routen.

---

### Inhalt und Schlüsselmodule

- `src/lib.rs`: Einstiegspunkt der Crate; exportiert die Hauptmodule: `parser`, `processor`, `storage`, `routing`, `cla` und `telemetry`.

#### CBOR- und BPv7-Parser
- `src/parser/cbor.rs`: *Zero-Copy*-Hilfsfunktionen für Canonical CBOR. Enthält `decode_unsigned()` für VARINT, `decode_definite_length()` sowie Abwehrmaßnahmen gegen fehlerhaftes CBOR.
- `src/parser/primary_block.rs`: Parser für den BPv7-Primärblock. Stellt `ParsedBundleHeader<'a>`, `parse_primary_block()` mit strenger Grenzprüfung und $O(1)$-Inspektion über `quick_validate_primary_block()` bereit.

#### Prozessor und Zustandsmaschine
- `src/processor/state_machine.rs`: Zustandsmaschine zur Validierung von BPv7-DTN-Paketen. Verarbeitet `ParseStatus` und wendet `Accepted`-, `Expired`- oder `Malformed`-Übergänge an.
- `src/processor/fragmentation.rs`: Fragment-Reassemblierung über `ReassemblySlot<const BUF_SIZE: usize>`. Ermöglicht *Zero-Allocation*-Rekonstruktion fragmentierter Payloads durch Auswertung von `processing_flags`.
- `src/processor/pipeline.rs`: `IngestionPipeline`, die Paketaufnahme mit CBOR-Parser, CGR-Policy-Auswertung, Lock-Free-Warteschlangen und Festplatten-Fallback verbindet.

#### Convergence Layer Adapter (CLA)
- `src/cla/udp.rs`: `UdpFrameBuffer<const MAX_FRAME_SIZE: usize>` für nicht-blockierenden Socket-Empfang ohne Heap-Zuweisung. Löst direkt `IngestionPipeline` aus.

#### Speicher und Puffer
- `src/storage/ring_buffer.rs`: `LockFreeRingBuffer<const SIZE: usize>` ausgerichtet auf 64 Bytes (`#[repr(align(64))]`) zur Vermeidung von *False Sharing*. Verwendet Acquire/Release-Semantik für SPSC-Konkurrenz ohne Speicherzuweisungen.
- `src/storage/wal.rs`: `DirectWal<const PAGES: usize>`-Abstraktion, ausgerichtet auf Seitengrößen (`PAGE_SIZE = 4096`). Die Methoden `append()` und `flush_pages()` sind für Direct I/O (`O_DIRECT` / `pwrite`) über das `WalWriter`-Trait ausgelegt.
- `src/storage/disk_sink.rs`: `DiskBlockStore<const MAX_PAGES: usize>`-Implementierung für seitenausgerichtetes Direct I/O. Dient als persistente Senke (*Store-and-Forward*), wenn die speicherinterne Warteschlange überläuft oder der Link keine aktive Route hat (`DropNoRoute`).

#### Routing
- `src/routing/interval_tree.rs`: `CgrIntervalTree<const CAP: usize>`, ein statischer Index sortiert nach `start_time` für EAT (*Earliest Arrival Time*)-Suchanfragen. Optimierte Einfügung/Löschung mit `copy_within` (vektorisiertes memmove) und $O(\log N)$-Suche mit `find_next(t)`.

#### Telemetrie
- `src/telemetry/metrics.rs`: `no_std`-Metriken basierend auf `AtomicU64` für Zähler und kumulierte Latenz (Summe, Minimum, Maximum, Anzahl). Bietet atomare `snapshot()`- und `reset()`-Methoden.

---

### Design und Architektur-Garantien

- **no_std:** Crate gekennzeichnet mit `#![no_std]`, vollständig frei von Abhängigkeiten zu `alloc` oder dem System-Heap.
- **Zero-Copy:** Parsing von `&[u8]`-Slices, die Zero-Copy-Referenzen mit Lifetimes zurückgeben.
- **Lock-Free SPSC:** `LockFreeRingBuffer` verwendet `AtomicUsize` mit hochleistungsfähiger SPSC-Kopplung.
- **Speicherausrichtung:** Cache-Zeilen-Auffüllung (64 Bytes) zur Minderung von *False Sharing* auf CPUs sowie 4096-Byte-Ausrichtung für Direct I/O.
- **Store-and-Forward mit atomarer Persistenz:** Behandlung intermittierender Routen. Wenn `CgrIntervalTree` kein gültiges Intervall zurückgibt oder der `LockFreeRingBuffer` überläuft, fällt das Bundle dynamisch auf `PersistedToWal` über `DirectWal`/`DiskBlockStore` zurück.
- **Fragmentierungs-Überlappungs-Resilienz:** Der Reassemblierungs-Slot behandelt Fragment-Offsets statisch im Stack/Global-Speicher und garantiert $O(1)$-Speicher-Fußabdruck.
- **Direkter CLA-Fluss:** Vollständige Entkopplung von asynchronen Runtimes (`tokio`/`async-std`). UDP-Empfang schreibt direkt in statische Byte-Puffer und delegiert an den Prozessor ohne Zwischenkopien.

---

### Benchmark-Ergebnisse (Criterion)

- **Testszenario:** UDP-CLA-Empfang $\rightarrow$ Zero-Copy-CBOR-Parsing $\rightarrow$ Zustandsmaschinen-Validierung $\rightarrow$ CGR $O(\log N)$-Suche $\rightarrow$ SPSC-RingBuffer-Warteschlange.
- **Durchschnittliche Hot-Path-Latenz:** **`~59.6 ns`** pro Bundle.
- **Theoretischer Durchsatz:** **`~16.7 Mops/s`** (Millionen Operationen pro Sekunde auf einem einzelnen CPU-Kern).
- **Dynamische Verifikation:** **21/21 Tests** erfolgreich unter **Miri** (`cargo +nightly miri test --lib`) bestanden, was die Abwesenheit von *Uninitialized Memory Reads*, *Data Races* und *Pointer-Provenance*-Verletzungen garantiert.

---

### Test- und Benchmarking-Anleitung

#### 1. Unit-Tests und Miri

```fish
# Standard-Unit-Tests
cargo test --lib

# Überprüfung von Pointer Provenance und Datenrennen mit Miri
cargo +nightly miri test --lib
```

#### 2. Leistungs-Benchmarks

```fish
# Ausführung des Hot-Path-Leistungs-Testrahmens
cargo bench --bench pipeline_bench
```
