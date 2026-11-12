use super::*;
fn points(queues: &Vst3ProcessQueues, index: usize) -> Vec<(int32, ParamValue)> {
    queues.value_queues[index]
        .points
        .borrow()
        .iter()
        .map(|&(offset, value, _)| (offset, value))
        .collect()
}
fn event(id: u32, sample_offset: i32, value: f64) -> Vst3ParamEvent {
    Vst3ParamEvent {
        id,
        sample_offset,
        value,
    }
}
fn prepared(ids: &[u32]) -> Vst3ProcessQueues {
    let mut queues = Vst3ProcessQueues::new();
    for id in ids {
        queues.queue_ids.push(*id);
        queues
            .value_queues
            .push(ComWrapper::new(SimpleParamValueQueue {
                id: *id,
                points: RefCell::new(Vec::with_capacity(8)),
            }));
    }
    queues.parameters.queues.borrow_mut().reserve(ids.len());
    queues.prepare_parameter_index();
    queues
}
#[test]
fn prepared_index_keeps_descriptor_queue_order_and_sorts_only_each_queue_offsets() {
    let mut queues = prepared(&[9, 2, 7]);
    let mut events = vec![
        event(7, 9, 0.7),
        event(9, 8, 0.9),
        event(7, 3, 0.3),
        event(9, 1, 0.1),
    ];
    queues.load_parameters(&mut events);
    assert!(events.is_empty());
    assert_eq!(queues.queue_ids, [9, 2, 7]);
    assert_eq!(&points(&queues, 0), &[(1, 0.1), (8, 0.9)]);
    assert_eq!(&points(&queues, 2), &[(3, 0.3), (9, 0.7)]);
    let actual: Vec<_> = queues
        .parameters
        .queues
        .borrow()
        .iter()
        .map(|queue| {
            // SAFETY: these host-owned COM queue references remain retained by queues.
            unsafe { queue.getParameterId() }
        })
        .collect();
    assert_eq!(actual, [9, 7]);
    assert_eq!(queues.packing_work.events, 4);
    assert_eq!(queues.packing_work.lookups, 4);
    assert_eq!(queues.packing_work.discovered, 0);
    assert!(queues.packing_work.comparisons <= 8);
}
#[test]
fn legacy_discovery_retains_first_seen_order_and_reuses_index_after_new_ids() {
    let mut queues = Vst3ProcessQueues::new();
    queues.load_parameters(&mut vec![
        event(7, 3, 0.7),
        event(3, 2, 0.3),
        event(7, 1, 0.1),
        event(9, 0, 0.9),
    ]);
    assert_eq!(queues.queue_ids, [7, 3, 9]);
    assert_eq!(queues.queue_index, [(3, 1), (7, 0), (9, 2)]);
    assert_eq!(queues.packing_work.discovered, 3);
    queues.load_parameters(&mut vec![event(9, 4, 0.4), event(3, 1, 0.1)]);
    assert_eq!(queues.queue_ids, [7, 3, 9]);
    assert_eq!(queues.packing_work.discovered, 0);
    assert!(queues.value_queues[0].points.borrow().is_empty());
    assert_eq!(&points(&queues, 1), &[(1, 0.1)]);
    assert_eq!(&points(&queues, 2), &[(4, 0.4)]);
}

#[test]
fn equal_offset_native_points_preserve_all_source_ordinals() {
    let mut queues = prepared(&[7]);
    let mut events = (0..128)
        .map(|i| event(7, 3, f64::from(i) / 128.0))
        .collect();
    queues.load_parameters(&mut events);
    assert_eq!(
        points(&queues, 0),
        (0..128)
            .map(|i| (3, f64::from(i) / 128.0))
            .collect::<Vec<_>>()
    );
}
