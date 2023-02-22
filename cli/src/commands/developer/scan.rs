// Copyright (C) 2019-2023 Aleo Systems Inc.
// This file is part of the snarkOS library.

// The snarkOS library is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The snarkOS library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with the snarkOS library. If not, see <https://www.gnu.org/licenses/>.

use super::CurrentNetwork;

use snarkvm::prelude::{Block, Ciphertext, Field, Network, Plaintext, PrivateKey, Record, ViewKey};

use anyhow::{bail, ensure, Result};
use clap::Parser;
use std::str::FromStr;

// TODO (raychu86): Figure out what to do with this naive scan. This scan currently does not check if records are already spent.
/// Scan the snarkOS node for records.
#[derive(Debug, Parser)]
pub struct Scan {
    /// An optional private key scan for unspent records.
    #[clap(short, long)]
    private_key: Option<String>,

    /// The view key used to scan for records.
    #[clap(short, long)]
    view_key: Option<String>,

    /// The block height to start scanning from.
    #[clap(long, default_value = "0")]
    start: u32,

    /// The block height to stop scanning.
    #[clap(long)]
    end: Option<u32>,

    /// The endpoint to scan blocks from.
    #[clap(long)]
    endpoint: String,
}

impl Scan {
    pub fn parse(self) -> Result<String> {
        // Derive the view key and optional private key.
        let (private_key, view_key) = self.parse_account()?;

        // Find the end height.
        let end = self.parse_end_height()?;

        // Fetch the records from the network.
        let records = Self::fetch_records(private_key, &view_key, &self.endpoint, self.start, end)?;

        // Output the decrypted records associated with the view key.
        if records.is_empty() {
            Ok("No records found".to_string())
        } else {
            if private_key.is_none() {
                println!("⚠️  This list may contain records that have already been spent.\n");
            }

            Ok(serde_json::to_string_pretty(&records)?.replace("\\n", ""))
        }
    }

    /// Returns the view key and optional private key, from the given configurations.
    fn parse_account<N: Network>(&self) -> Result<(Option<PrivateKey<N>>, ViewKey<N>)> {
        match (&self.private_key, &self.view_key) {
            (Some(private_key), Some(view_key)) => {
                // Derive the private key.
                let private_key = PrivateKey::<N>::from_str(private_key)?;
                // Derive the expected view key.
                let expected_view_key = ViewKey::<N>::try_from(private_key)?;
                // Derive the view key.
                let view_key = ViewKey::<N>::from_str(view_key)?;

                ensure!(
                    expected_view_key == view_key,
                    "The provided private key does not correspond to the provided view key."
                );

                Ok((Some(private_key), view_key))
            }
            (Some(private_key), _) => {
                // Derive the private key.
                let private_key = PrivateKey::<N>::from_str(private_key)?;
                // Derive the view key.
                let view_key = ViewKey::<N>::try_from(private_key)?;

                Ok((Some(private_key), view_key))
            }
            (None, Some(view_key)) => Ok((None, ViewKey::<N>::from_str(view_key)?)),
            (None, None) => bail!("Missing private key or view key."),
        }
    }

    /// Returns latest block hash to request.
    fn parse_end_height(&self) -> Result<u32> {
        // Find the end height.
        let end = match self.end {
            Some(height) => height,
            None => {
                // Request the latest block height from the endpoint.
                let endpoint = format!("{}/testnet3/latest/height", self.endpoint);
                let latest_height = u32::from_str(&ureq::get(&endpoint).call()?.into_string()?)?;

                // Print warning message if the user is attempting to scan the whole chain.
                if self.start == 0 {
                    println!("⚠️  Attention - Scanning the entire chain. This may take a few minutes...\n");
                }

                latest_height
            }
        };

        ensure!(end > self.start, "The given scan range is invalid (start = {}, end = {end})", self.start);

        Ok(end)
    }

    /// Fetch owned ciphertext records from the endpoint.
    pub fn fetch_records(
        private_key: Option<PrivateKey<CurrentNetwork>>,
        view_key: &ViewKey<CurrentNetwork>,
        endpoint: &str,
        start_height: u32,
        end_height: u32,
    ) -> Result<Vec<Record<CurrentNetwork, Plaintext<CurrentNetwork>>>> {
        // Check the bounds of the request.
        if start_height > end_height {
            bail!("Invalid block range");
        }

        // Derive the x-coordinate of the address corresponding to the given view key.
        let address_x_coordinate = view_key.to_address().to_x_coordinate();

        const MAX_BLOCK_RANGE: u32 = 50;

        let mut records = Vec::new();

        // Scan the endpoint starting from the start height
        let mut request_start = start_height;
        while request_start <= end_height {
            // TODO (raychu86): Add progress bar.

            let num_blocks_to_request =
                std::cmp::min(MAX_BLOCK_RANGE, end_height.saturating_sub(request_start).saturating_add(1));
            let request_end = request_start.saturating_add(num_blocks_to_request);

            // Establish the endpoint.
            let blocks_endpoint = format!("{endpoint}/testnet3/blocks?start={request_start}&end={request_end}");

            // Fetch blocks
            let blocks: Vec<Block<CurrentNetwork>> = ureq::get(&blocks_endpoint).call()?.into_json()?;

            // Scan the blocks for owned records.
            for block in &blocks {
                for (commitment, ciphertext_record) in block.records() {
                    // Check if the record is owned by the given view key.
                    if ciphertext_record.is_owner_with_address_x_coordinate(view_key, &address_x_coordinate) {
                        // Decrypt and optionally filter the records.
                        if let Some(record) =
                            Self::decrypt_record(private_key, view_key, endpoint, *commitment, ciphertext_record)?
                        {
                            records.push(record);
                        }
                    }
                }
            }

            request_start = request_start.saturating_add(num_blocks_to_request);
        }

        Ok(records)
    }

    /// Decrypts the ciphertext record and filters spend record if a private key was provided.
    fn decrypt_record(
        private_key: Option<PrivateKey<CurrentNetwork>>,
        view_key: &ViewKey<CurrentNetwork>,
        endpoint: &str,
        commitment: Field<CurrentNetwork>,
        ciphertext_record: &Record<CurrentNetwork, Ciphertext<CurrentNetwork>>,
    ) -> Result<Option<Record<CurrentNetwork, Plaintext<CurrentNetwork>>>> {
        // Check if a private key was provided.
        if let Some(private_key) = private_key {
            // Compute the serial number.
            let serial_number =
                Record::<CurrentNetwork, Plaintext<CurrentNetwork>>::serial_number(private_key, commitment)?;

            // Establish the endpoint.
            let endpoint = format!("{endpoint}/testnet3/find/transitionID/{serial_number}");

            // Check if the record is spent.
            match ureq::get(&endpoint).call() {
                // On success, skip as the record is spent.
                Ok(_) => Ok(None),
                // On error, add the record.
                Err(_error) => {
                    // TODO: Dedup the error types. We're adding the record as valid because the endpoint failed,
                    //  meaning it couldn't find the serial number (ie. unspent). However if there's a DNS error or request error,
                    //  we have a false positive here then.
                    // Decrypt the record.
                    Ok(Some(ciphertext_record.decrypt(view_key)?))
                }
            }
        } else {
            // If no private key was provided, return the record.
            Ok(Some(ciphertext_record.decrypt(view_key)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snarkvm::prelude::{TestRng, Testnet3};

    type CurrentNetwork = Testnet3;

    #[test]
    fn test_parse_account() {
        let rng = &mut TestRng::default();

        // Generate private key and view key.
        let private_key = PrivateKey::<CurrentNetwork>::new(rng).unwrap();
        let view_key = ViewKey::try_from(private_key).unwrap();

        // Generate unassociated private key and view key.
        let unassociated_private_key = PrivateKey::<CurrentNetwork>::new(rng).unwrap();
        let unassociated_view_key = ViewKey::try_from(unassociated_private_key).unwrap();

        let config = Scan::try_parse_from(
            [
                "snarkos",
                "--private-key",
                &format!("{private_key}"),
                "--view-key",
                &format!("{view_key}"),
                "--start",
                "0",
                "--end",
                "10",
                "--endpoint",
                "",
            ]
            .iter(),
        )
        .unwrap();
        assert!(config.parse_account::<CurrentNetwork>().is_ok());

        let config = Scan::try_parse_from(
            [
                "snarkos",
                "--private-key",
                &format!("{private_key}"),
                "--view-key",
                &format!("{unassociated_view_key}"),
                "--start",
                "0",
                "--end",
                "10",
                "--endpoint",
                "",
            ]
            .iter(),
        )
        .unwrap();
        assert!(config.parse_account::<CurrentNetwork>().is_err());
    }

    #[test]
    fn test_parse_end_height() {
        let config =
            Scan::try_parse_from(["snarkos", "--view-key", "", "--start", "0", "--end", "10", "--endpoint", ""].iter())
                .unwrap();
        assert!(config.parse_end_height().is_ok());

        let config =
            Scan::try_parse_from(["snarkos", "--view-key", "", "--start", "10", "--end", "5", "--endpoint", ""].iter())
                .unwrap();
        assert!(config.parse_end_height().is_err());
    }
}
