use bittorrent_core::{Session, SessionConfig};
use turmoil::Builder;

#[test]
fn transfer_simulation() -> turmoil::Result {
    let mut sim = Builder::new().build();

    sim.host("seeder", || async { Ok(()) });

    sim.run()
}
