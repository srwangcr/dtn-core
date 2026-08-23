# dtn-core -- Motor DTN (BPv7) Zero-Allocation y no_std

dtn-core es una implementacion de componentes centrales para un motor DTN (Bundle Protocol v7, RFC 9171) disenada para entornos bare-metal y #![no_std] de alto rendimiento.

---

## Indice / Index

* Espanol (Principal)
* English
* Chinese
* Deutsch

---

## Espanol

### Pilares de Arquitectura

* Zero-Allocation y Zero-Copy: Procesamiento estricto sobre slices de memoria sin tocar el heap.


* Lock-Free SPSC: RingBuffers y colas alineadas a la cache L1 (64 bytes) para evitar false sharing.


* Verificacion Formal: Integridad de memoria validada via Miri (libre de UB y data races).


* Store-and-Forward Atomico: Persistencia directa a disco con Direct I/O (O_DIRECT / pwrite) para enlaces intermitentes.



---

### Benchmarks de Rendimiento (Criterion)

Evaluacion del hot-path de ingesta E2E (UDP CLA -> CBOR Zero-Copy Parser -> State Machine -> CGR Interval Tree -> SPSC Lock-Free RingBuffer):

| Metrica | Resultado |
| --- | --- |
| Latencia Media Hot-Path | 59.63 ns

 |
| Throughput Teorico | ~16.7 Mops/sec

 |
| Garantia de Memoria | 0 asignaciones en Heap (0 bytes)

 |
| Verificacion Formal | 21/21 Tests Miri Compliant (Zero UB / No Data Races)

 |

---

### Modulos del Sistema

* src/lib.rs: Entrypoint del crate no_std.


* src/parser/cbor.rs: Decodificacion Zero-Copy Canonical CBOR (VARINT / Definite Length).


* src/parser/primary_block.rs: Parser O(1) de Bloque Primario BPv7 (ParsedBundleHeader).


* src/processor/state_machine.rs: Transiciones de estado (Accepted, Expired, Malformed).


* src/processor/fragmentation.rs: Reensamblado O(1) en stack (ReassemblySlot).


* src/processor/pipeline.rs: Canalizacion IngestionPipeline.


* src/cla/udp.rs: UdpFrameBuffer para ingesta Socket no bloqueante.


* src/storage/ring_buffer.rs: LockFreeRingBuffer SPSC #[repr(align(64))].


* src/storage/wal.rs: DirectWal alineado a pagina (PAGE_SIZE = 4096).


* src/storage/disk_sink.rs: DiskBlockStore (Persistencia Direct I/O Store-and-Forward).


* src/routing/interval_tree.rs: CgrIntervalTree (Indice estatico EAT, busqueda O(log N)).


* src/telemetry/metrics.rs: Contadores no_std basados en AtomicU64.



---

### Guia de Pruebas y Uso Rapido

1. Verificacion y Benchmarks

cargo test --lib
cargo +nightly miri test --lib
cargo bench --bench pipeline_bench

2. Daemon y Control CLI

cargo run --bin dtnd
cargo run --bin dtn-cli send 127.0.0.1:4556 "Space Payload"
cargo run --bin chaos_injector

3. Despliegue con Docker (< 3 MB)

docker build -t dtn-core:v0.1.0 .
docker run -d -p 4556:4556/udp dtn-core:v0.1.0

---

## English

dtn-core is an implementation of core components for a DTN engine (Bundle Protocol v7, RFC 9171) designed for bare-metal and no_std high-performance environments.

### Key Features

* Strict Zero-Allocation and Zero-Copy in the hot-path.


* Cache-aligned Lock-Free SPSC queue and buffer operations.


* Formal Verification Guarantees (Miri compliant, free of UB and data races).


* Store-and-Forward with atomic persistence for intermittent routes.



### Performance Benchmarks

| Metric | Result |
| --- | --- |
| Hot-Path Average Latency | 59.63 ns

 |
| Theoretical Throughput | ~16.7 Mops/sec

 |
| Memory Guarantee | 0 Heap Allocations (0 bytes)

 |
| Formal Verification | 21/21 Miri Compliant Tests

 |

---

## Chinese

dtn-core 是一个为高性能裸机和 no_std 环境设计的 DTN 引擎（Bundle Protocol v7，RFC 9171）核心组件实现。

### 核心特性

* 热路径中的严格零分配和零拷贝。


* 缓存行对齐的无锁 SPSC 队列和缓冲区操作。


* 形式验证保证（兼容 Miri，无未定义行为和数据竞争）。


* 具有原子持久性的存储转发，用于间歇性路由。



### 基准测试结果

| 指标 | 结果 |
| --- | --- |
| 热路径平均延迟 | 59.63 ns

 |
| 理论吞吐量 | ~16.7 百万 ops/sec

 |
| 内存保证 | 0 堆分配 (0 字节)

 |
| 形式验证 | 21/21 Miri 兼容测试

 |

---

## Deutsch

dtn-core ist eine Implementierung von Kernkomponenten fur eine DTN-Engine (Bundle Protocol v7, RFC 9171), die fur Bare-Metal- und no_std-Hochleistungsumgebungen entwickelt wurde.

### Hauptmerkmale

* Strikte Zero-Allocation und Zero-Copy im Hot-Path.


* Cache-alignte Lock-Free SPSC-Operationen fur Warteschlangen und Puffer.


* Formale Verifikationsgarantien (Miri-konform, frei von UB und Datenrennen).


* Store-and-Forward mit atomarer Persistenz fur intermittierende Routen.



### Benchmark-Ergebnisse

| Metrik | Ergebnis |
| --- | --- |
| Durchschnittliche Hot-Path-Latenz | 59.63 ns

 |
| Theoretischer Durchsatz | ~16.7 Millionen ops/sec

 |
| Speichergarantie | 0 Heap-Zuweisungen (0 Bytes)

 |
| Formale Verifikation | 21/21 Miri-konforme Tests

 |