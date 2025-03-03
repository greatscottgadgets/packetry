//! Helper type for managing a queue of USB bulk transfers.

use std::sync::mpsc;

use anyhow::{Context, Error};
use futures_channel::oneshot;
use futures_util::{future::FusedFuture, FutureExt, select_biased};
use nusb::{Endpoint, transfer::{Completion, Bulk, In}};

/// A queue of inbound USB transfers, feeding received data to a channel.
pub struct TransferQueue {
    endpoint: Endpoint<Bulk, In>,
    data_tx: mpsc::Sender<Completion<Bulk, In>>,
    transfer_length: usize,
}

impl TransferQueue {
    /// Create a new transfer queue.
    pub fn new(
        mut endpoint: Endpoint<Bulk, In>,
        data_tx: mpsc::Sender<Completion<Bulk, In>>,
        num_transfers: usize,
        transfer_length: usize
    ) -> TransferQueue {
        while endpoint.pending() < num_transfers {
            let request = endpoint.allocate(transfer_length);
            endpoint.submit(request);
        }
        TransferQueue { endpoint, data_tx, transfer_length }
    }

    /// Process the queue, sending data to the channel until stopped.
    pub async fn process(&mut self, mut stop_rx: oneshot::Receiver<()>)
        -> Result<(), Error>
    {
        use nusb::transfer::TransferError::Cancelled;

        loop {
            select_biased!(
                _ = stop_rx => {
                    // Stop requested. Cancel all transfers.
                    self.endpoint.cancel_all();
                }
                completion = self.endpoint.next_complete().fuse() => {
                    match completion.status() {
                        Ok(()) => {

                            // Send data to decoder thread.
                            self.data_tx.send(completion)
                                .context(
                                    "Failed sending capture data to channel")?;
                            if !stop_rx.is_terminated() {
                                // Submit next transfer.
                                let request = self.endpoint
                                    .allocate(self.transfer_length);
                                self.endpoint.submit(request);
                            }
                        },
                        Err(Cancelled) if stop_rx.is_terminated() => {
                            // Transfer cancelled during shutdown. Drop it.
                            drop(completion);
                            if self.endpoint.pending() == 0 {
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
