//! Helper type for managing a queue of USB bulk transfers.

use std::sync::mpsc;

use anyhow::{Context, Error};
use futures_channel::oneshot;
use futures_util::{future::FusedFuture, FutureExt, select_biased};
use nusb::{Interface, transfer::{Queue, RequestBuffer, TransferError}};

/// A queue of inbound USB transfers, feeding received data to a channel.
pub struct TransferQueue {
    queue: Queue<RequestBuffer>,
    data_tx: mpsc::Sender<Vec<u8>>,
    transfer_length: usize,
}

impl TransferQueue {
    /// Create a new transfer queue.
    pub fn new(
        interface: &Interface,
        data_tx: mpsc::Sender<Vec<u8>>,
        endpoint: u8,
        num_transfers: usize,
        transfer_length: usize
    ) -> TransferQueue {
        let mut queue = interface.bulk_in_queue(endpoint);
        while queue.pending() < num_transfers {
            queue.submit(RequestBuffer::new(transfer_length));
        }
        TransferQueue { queue, data_tx, transfer_length }
    }

    /// Process the queue, sending data to the channel until stopped.
    pub async fn process(&mut self, mut stop_rx: oneshot::Receiver<()>)
        -> Result<(), Error>
    {
        use TransferError::Cancelled;
        loop {
            select_biased!(
                _ = stop_rx => {
                    // Stop requested. Cancel all transfers.
                    self.queue.cancel_all();
                }
                completion = self.queue.next_complete().fuse() => {
                    match completion.status {
                        Ok(()) => {
                            // Send data to decoder thread.
                            self.data_tx.send(completion.data)
                                .context(
                                    "Failed sending capture data to channel")?;
                            if !stop_rx.is_terminated() {
                                // Submit next transfer.
                                self.queue.submit(
                                    RequestBuffer::new(self.transfer_length)
                                );
                            }
                        },
                        Err(Cancelled) if stop_rx.is_terminated() => {
                            // Transfer cancelled during shutdown. Drop it.
                            drop(completion);
                            if self.queue.pending() == 0 {
                                // All cancellations now handled.
                                return Ok(());
                            }
                        },
                        Err(usb_error) => {
                            // Transfer failed.
                            return Err(Error::from(usb_error));
                        }
                    }
                }
            );
        }
    }
}
