//! Structured-tracing initialisation shared by the `ch-control` and `ch-agent`
//! binaries.
//!
//! Every service emits structured logs via `tracing`. Log verbosity is taken
//! from the `RUST_LOG` environment variable, falling back to the level the
//! service passes in. Output can be human-readable text or JSON (design M17),
//! and spans can additionally be exported to an OpenTelemetry collector over
//! OTLP/gRPC when an endpoint is configured.

use tracing_subscriber::{fmt, prelude::*, EnvFilter, Layer, Registry};

type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// Initialise the global tracing subscriber.
///
/// `default_level` is used when `RUST_LOG` is unset (e.g. `"info"`). When
/// `json` is true, logs are emitted as structured JSON lines. When
/// `otlp_endpoint` is set (e.g. `http://collector:4317`), spans are also
/// exported to that OpenTelemetry collector over OTLP/gRPC, tagged with
/// `service.name = service_name` (design M17). Safe to call once per process; a
/// second call is ignored.
pub fn init(default_level: &str, json: bool, otlp_endpoint: Option<&str>, service_name: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    let mut layers: Vec<BoxedLayer> = Vec::new();
    if json {
        layers.push(
            fmt::layer()
                .json()
                .with_target(true)
                .with_current_span(true)
                .boxed(),
        );
    } else {
        layers.push(fmt::layer().with_target(true).with_level(true).boxed());
    }

    if let Some(endpoint) = otlp_endpoint {
        match otlp_layer(endpoint, service_name) {
            Ok(layer) => layers.push(layer),
            // Don't fail startup if the collector is unreachable at boot — log
            // to stderr and carry on with local logging only.
            Err(e) => eprintln!("OTLP tracing export disabled: {e}"),
        }
    }

    // Ignore the error when a subscriber is already installed (e.g. repeated
    // initialisation across tests).
    let _ = tracing_subscriber::registry()
        .with(layers)
        .with(filter)
        .try_init();
}

/// Build an OpenTelemetry OTLP/gRPC span-export layer for `endpoint`.
fn otlp_layer(
    endpoint: &str,
    service_name: &str,
) -> Result<BoxedLayer, Box<dyn std::error::Error>> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig as _;

    // W3C traceparent propagation, so context can flow across services later.
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(service_name.to_string())
                .build(),
        )
        .build();

    let tracer = provider.tracer("ch-orchestrator");
    // Keep the provider installed globally so spans flush for the process life.
    opentelemetry::global::set_tracer_provider(provider);

    Ok(tracing_opentelemetry::layer().with_tracer(tracer).boxed())
}
