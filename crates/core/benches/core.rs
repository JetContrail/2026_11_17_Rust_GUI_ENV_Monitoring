#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use envmon_core::demo;
use envmon_core::model::ObjectId;
use envmon_core::objects::{MAX_OBJECTS, ObjectKind, ObjectRegistry, ObjectValue};
use envmon_core::{Level, demo::ROOM_DOOR};

fn bench_authorize(c: &mut Criterion) {
    let site = demo::site(200_000);
    let at = demo::epoch() + chrono::TimeDelta::days(30);
    let mut n = 0_u32;
    c.bench_function("authorize_door among 200k users", |b| {
        b.iter(|| {
            n = n % 200_000 + 1;
            black_box(site.directory.authorize_door(&demo::card(n), ROOM_DOOR, at))
        })
    });
}

fn bench_objects(c: &mut Criterion) {
    let mut reg = ObjectRegistry::new();
    for i in 0..MAX_OBJECTS as u32 {
        reg.register(ObjectId(i), ObjectKind::AnalogSensor, format!("s{i}"))
            .unwrap();
    }
    let mut t = 0.0_f32;
    c.bench_function("update + collect 5000 objects (one frame)", |b| {
        b.iter(|| {
            t += 0.1;
            let v = reg.version();
            for i in 0..MAX_OBJECTS as u32 {
                let value = ObjectValue::Analog {
                    value: t + i as f32,
                    level: Level::Normal,
                };
                reg.update(ObjectId(i), value).unwrap();
            }
            black_box(reg.changed_since(v).count())
        })
    });
}

criterion_group!(benches, bench_authorize, bench_objects);
criterion_main!(benches);
