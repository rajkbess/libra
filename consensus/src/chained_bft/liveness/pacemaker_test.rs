// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    chained_bft::{
        consensus_types::timeout_msg::PacemakerTimeout,
        liveness::{
            pacemaker::{
                ExponentialTimeInterval, NewRoundEvent, NewRoundReason, Pacemaker,
                PacemakerTimeInterval,
            },
            pacemaker_timeout_manager::HighestTimeoutCertificates,
        },
        persistent_storage::PersistentStorage,
        test_utils::{MockStorage, TestPayload},
    },
    util::mock_time_service::SimulatedTimeService,
};
use channel;
use crypto::ed25519::*;
use futures::{executor::block_on, StreamExt};
use std::{sync::Arc, time::Duration, u64};
use types::validator_signer::ValidatorSigner;

#[test]
fn test_pacemaker_time_interval() {
    let interval = ExponentialTimeInterval::new(Duration::from_millis(3000), 1.5, 2);
    assert_eq!(3000, interval.get_round_duration(0).as_millis());
    assert_eq!(4500, interval.get_round_duration(1).as_millis());
    assert_eq!(
        6750, /* 4500*1.5 */
        interval.get_round_duration(2).as_millis()
    );
    // Test that there is no integer overflow
    assert_eq!(6750, interval.get_round_duration(1000).as_millis());
}

#[test]
/// Verify that Pacemaker properly outputs PacemakerTimeoutMsg upon timeout
fn test_basic_timeout() {
    let time_interval = Box::new(ExponentialTimeInterval::fixed(Duration::from_millis(2)));
    let highest_certified_round = 1;
    let simulated_time = SimulatedTimeService::auto_advance_until(Duration::from_millis(4));
    let (new_round_events_sender, _new_round_events_receiver) = channel::new_test(1_024);
    let (external_timeout_sender, mut external_timeout_receiver) = channel::new_test(1_024);
    let mut pm = Pacemaker::new(
        MockStorage::<TestPayload>::start_for_testing()
            .0
            .persistent_liveness_storage(),
        time_interval,
        0,
        highest_certified_round,
        Arc::new(simulated_time.clone()),
        new_round_events_sender,
        external_timeout_sender,
        1,
        HighestTimeoutCertificates::new(None, None),
    );

    for _ in 0..2 {
        let round = block_on(external_timeout_receiver.next()).unwrap();
        // Here we just test timeout send retry,
        // round for timeout is not changed as no timeout certificate was gathered at this point
        assert_eq!(2, round);
        pm.process_local_timeout(round);
    }
}

#[test]
/// Verify that Pacemaker forms a timeout certificate on receiving sufficient timeout messages
fn test_timeout_certificate() {
    let rounds = 5;
    let mut signers: Vec<ValidatorSigner<Ed25519PrivateKey>> = vec![];
    for round in 1..rounds {
        let signer = ValidatorSigner::<Ed25519PrivateKey>::random([round as u8; 32]);
        signers.push(signer);
    }
    let (mut pm, mut new_round_events_receiver) = make_pacemaker();

    block_on(async move {
        // Wait for the initial event for the first round.
        expect_qc(1, &mut new_round_events_receiver).await;

        // Send timeout for rounds 1..5, each from a different author, so that they can be
        // accumulated into single timeout certificate
        for round in 1..rounds {
            let signer = &signers[round - 1];
            let pacemaker_timeout = PacemakerTimeout::new(round as u64, signer, None);
            pm.process_remote_timeout(pacemaker_timeout).await;
        }
        // Then timeout quorum for previous round (1,2,3) generates new round event for round 2
        expect_timeout(2, &mut new_round_events_receiver).await;
        // Then timeout quorum for previous round (2,3,4) generates new round event for round 3
        expect_timeout(3, &mut new_round_events_receiver).await;
    });
}

#[test]
fn test_basic_qc() {
    let (mut pm, mut new_round_events_receiver) = make_pacemaker();

    block_on(async move {
        // Wait for the initial event for the first round.
        expect_qc(1, &mut new_round_events_receiver).await;

        pm.process_certificates(2, None, None).await;
        pm.process_certificates(3, None, None).await;

        expect_qc(3, &mut new_round_events_receiver).await;
        expect_qc(4, &mut new_round_events_receiver).await;
    });
}

fn make_pacemaker() -> (Pacemaker, channel::Receiver<NewRoundEvent>) {
    let time_interval = Box::new(ExponentialTimeInterval::fixed(Duration::from_millis(2)));
    let simulated_time = SimulatedTimeService::new();
    let (new_round_events_sender, new_round_events_receiver) = channel::new_test(1_024);
    let (pacemaker_timeout_tx, _) = channel::new_test(1_024);
    (
        Pacemaker::new(
            MockStorage::<TestPayload>::start_for_testing()
                .0
                .persistent_liveness_storage(),
            time_interval,
            0,
            0,
            Arc::new(simulated_time.clone()),
            new_round_events_sender,
            pacemaker_timeout_tx,
            3,
            HighestTimeoutCertificates::new(None, None),
        ),
        new_round_events_receiver,
    )
}

async fn expect_qc(round: u64, rx: &mut channel::Receiver<NewRoundEvent>) {
    let event: NewRoundEvent = rx.next().await.unwrap();
    assert_eq!(round, event.round);
    assert_eq!(NewRoundReason::QCReady, event.reason);
}

async fn expect_timeout(round: u64, rx: &mut channel::Receiver<NewRoundEvent>) {
    let event: NewRoundEvent = rx.next().await.unwrap();
    assert_eq!(round, event.round);
    match event.reason {
        NewRoundReason::Timeout { .. } => (),
        x => panic!("Expected timeout for round {}, got {:?}", round, x),
    };
}
