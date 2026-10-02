use book::{BookState, VenueBook};
use common::Side;
use simulation::{Scenario, scenarios::generate};
#[test]
fn generated_markets_are_valid_ordered_l2_streams() {
    for scenario in Scenario::ALL {
        let events = generate(scenario);
        let mut books = [1_u16, 2, 3].map(|v| {
            let first = events.iter().find(|e| e.venue.0 == v).unwrap();
            VenueBook::<128>::new(first.venue, first.instrument)
        });
        let mut sequences = [0_u64; 3];
        let mut last = 0;
        for e in &events {
            // Strictly increasing local time makes direct and merged-replay order identical.
            assert!(e.receive_ts.0 > last);
            last = e.receive_ts.0;
            let v = usize::from(e.venue.0 - 1);
            sequences[v] += 1;
            assert_eq!(e.sequence, sequences[v]);
            books[v].apply(e).unwrap();
            if books[v].state() == BookState::Live {
                let (bid, ask) = (
                    books[v].best(Side::Buy).unwrap(),
                    books[v].best(Side::Sell).unwrap(),
                );
                assert!(bid.unwrap().price < ask.unwrap().price);
            }
        }
        assert!(books.iter().all(|b| b.state() == BookState::Live));
    }
}
