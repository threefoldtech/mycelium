use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;
use tokio::sync::oneshot;

use crate::router::Router;

#[derive(Clone, Debug)]
pub struct Timer {
    duration: Arc<Mutex<Duration>>,
    cancel_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl Timer {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration: Arc::new(Mutex::new(duration)),
            cancel_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn reset(&self, duration: Duration) {
        let mut cancel_tx = self.cancel_tx.lock().unwrap();
        if let Some(tx) = cancel_tx.take() {
            let _ = tx.send(());
        }
        *self.duration.lock().unwrap() = duration;
    }
    
    // on_expire is of a type that implements the Fn trait, which means it's a function or a closure. 
    // The + Send + 'static part means this function must also satisfy the Send trait (meaning it can be sent between threads safely) 
    // and have a 'static lifetime (meaning it doesn't capture any non-'static (i.e., temporary) values from its environment). 
    // This is necessary because the function could be called at any point in the future, 
    // potentially long after the context it was created in has gone away.

    async fn run(&self, on_expire: impl Fn() + Send + 'static) {
        loop {
            let (tx, rx) = oneshot::channel();
            *self.cancel_tx.lock().unwrap() = Some(tx);
            let duration = *self.duration.lock().unwrap();
            tokio::select! {
                _ = sleep(duration) => {
                    on_expire();
                }
                _ = rx => {}
            }
        }
    }

    pub fn new_ihu_timer(ihu_interval: u64) -> Timer{

        let timer = Timer::new(Duration::from_secs(ihu_interval));

        {
            let timer = timer.clone();

            tokio::spawn(async move {
                timer.run(|| {
                    println!("IHU Timer expired!");
                    // node should be assumed as dead
                    // on expiry the neighbour link should be set to 0xFFFF
                    // and the route selection process should be run
                }).await;
            });
        }

        timer
    }

}
