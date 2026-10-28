use crossbeam_channel::unbounded;
use std::thread;
use std::time::Duration;

fn main() {
    let (mut tx, rx) = unbounded::<i32>();
    let handle = thread::spawn(move || {
        loop {
            match rx.recv() {
                Ok(v) => println!("received {}", v),
                Err(_) => { println!("disconnected"); break; }
            }
        }
    });

    tx.send(1).unwrap();
    let (dummy_tx, _) = unbounded();
    let real_tx = std::mem::replace(&mut tx, dummy_tx);
    drop(real_tx);
    handle.join().unwrap();
    println!("Done");
}
