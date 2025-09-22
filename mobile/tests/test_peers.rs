use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{thread, env};
use mobile::{start_mycelium, stop_mycelium, get_peer_status};

// Tests the Windows-specific issue with start_mycelium not completing after stop command
#[test]
fn test_peers() {
    

    // Initialize logging
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .try_init();

    // Generate test key
    let test_key = vec![0; 32];

    // Flag to track if mycelium completed
    let completed = Arc::new(AtomicBool::new(false));
    let completed_clone = Arc::clone(&completed);
    
    // Start mycelium in a separate OS thread to avoid tokio runtime conflicts
    let handle = thread::spawn(move || {
        println!("Starting mycelium");
        // Run mycelium
        start_mycelium(vec!["tcp://188.40.132.242:9651".to_string(), ], -1, test_key);
        // If we get here, mycelium has fully exited
        completed_clone.store(true, Ordering::SeqCst);
        println!("Mycelium function returned");
    });
    
    // Wait for mycelium to initialize
    thread::sleep(Duration::from_secs(2));
    
    // Send stop command
    println!("Sending stop command");
    let response = get_peer_status();
    println!("Peer status response: {:?}", response);
    let response = stop_mycelium();
    println!("Stop command response: {:?}", response);
    
    // Wait to see if mycelium completes after stop
    for i in 1..=100 {
        if completed.load(Ordering::SeqCst) {
            println!("Mycelium exited successfully");
            break;
        }
        println!("Waiting for mycelium to exit... ({}/100)", i);
        thread::sleep(Duration::from_secs(1));
    }
    
    // Check if mycelium ever completed
        assert!(completed.load(Ordering::SeqCst), " Mycelium did not exit after stop command")

    // Don't wait for the thread indefinitely
    // In the Windows bug case, this will exit without the thread completing
}