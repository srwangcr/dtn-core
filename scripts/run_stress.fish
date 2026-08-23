#!/usr/bin/env fish

echo "=================================================="
echo "   DTN-Core: Suite Completa de Estrés y Métricas  "
echo "=================================================="

echo "[1/4] Ejecutando Tests de Integración Pipeline..."
cargo test --test stress_pipeline -- --nocapture
if test $status -ne 0
    echo "❌ Error en tests de integración."
    exit 1
end

echo "\n[2/4] Corriendo Benchmark Multi-hilo de SPSC RingBuffer (Release)..."
cargo run --release --bench spsc_bench
if test $status -ne 0
    echo "❌ Error ejecutando benchmark."
    exit 1
end

echo "\n[3/4] Corriendo Miri en Búsqueda de Data Races / UB..."
cargo +nightly miri test --lib
if test $status -ne 0
    echo "❌ Miri detectó Undefined Behavior."
    exit 1
end

echo "\n=================================================="
echo "🚀 ¡TODAS LAS PRUEBAS DE ESTRÉS PASARON CON ÉXITO!"
echo "=================================================="