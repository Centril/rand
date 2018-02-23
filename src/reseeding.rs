// Copyright 2013 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// https://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A wrapper around another PRNG that reseeds it after it
//! generates a certain number of random bytes.

use {RngCore, SeedableRng, Error, ErrorKind};

/// A wrapper around any PRNG which reseeds the underlying PRNG after it has
/// generated a certain number of random bytes.
///
/// Reseeding is never strictly *necessary*. Cryptographic PRNGs don't have a
/// limited number of bytes they can output, or at least not a limit reachable
/// in any practical way. There is no such thing as 'running out of entropy'.
///
/// Some small non-cryptographic PRNGs can have very small periods, for
/// example less than 2<sup>64</sup>. Would reseeding help to ensure that you do
/// not wrap around at the end of the period? A period of 2<sup>64</sup> still
/// takes several centuries of CPU-years on current hardware. Reseeding will
/// actually make things worse, because the reseeded PRNG will just continue
/// somewhere else *in the same period*, with a high chance of overlapping with
/// previously used parts of it.
///
/// # When should you use `ReseedingRng`?
///
/// - Reseeding can be seen as some form of 'security in depth'. Even if in the
///   future a cryptographic weakness is found in the CSPRNG being used,
///   occasionally reseeding should make exploiting it much more difficult or
///   even impossible.
/// - It can be used as a poor man's cryptography (not recommended, just use a
///   good CSPRNG). Previous implementations of `thread_rng` for example used
///   `ReseedingRng` with the ISAAC RNG. That algorithm, although apparently
///   strong and with no known attack, does not come with any proof of security
///   and does not meet the current standards for a cryptographically secure
///   PRNG. By reseeding it frequently (every 32 MiB) it seems safe to assume
///   there is no attack that can operate on the tiny window between reseeds.
///
/// # Error handling
///
/// If reseeding fails, `try_fill_bytes` is the only `Rng` method to report it.
/// For all other `Rng` methods, `ReseedingRng` will not panic but try to
/// handle the error intelligently; if handling the source error fails these
/// methods will continue generating data from the wrapped PRNG without
/// reseeding.
///
/// It is usually best to use the infallible methods `next_u32`, `next_u64` and
/// `fill_bytes` because they can make use of this error handling strategy.
/// Use `try_fill_bytes` and possibly `try_reseed` if you want to handle
/// reseeding errors explicitly.
#[derive(Debug)]
pub struct ReseedingRng<R, Rsdr> {
    rng: R,
    reseeder: Rsdr,
    threshold: i64,
    bytes_until_reseed: i64,
}

impl<R: RngCore + SeedableRng, Rsdr: RngCore> ReseedingRng<R, Rsdr> {
    /// Create a new `ReseedingRng` with the given parameters.
    ///
    /// # Arguments
    ///
    /// * `rng`: the random number generator to use.
    /// * `threshold`: the number of generated bytes after which to reseed the RNG.
    /// * `reseeder`: the RNG to use for reseeding.
    pub fn new(rng: R, threshold: u64, reseeder: Rsdr) -> ReseedingRng<R,Rsdr> {
        assert!(threshold <= ::core::i64::MAX as u64);
        ReseedingRng {
            rng: rng,
            reseeder: reseeder,
            threshold: threshold as i64,
            bytes_until_reseed: threshold as i64,
        }
    }

    /// Reseed the internal PRNG.
    ///
    /// This will try to work around errors in the RNG used for reseeding
    /// intelligently. If the error kind indicates retrying might help, it will
    /// immediately retry a couple of times. If the error kind indicates the
    /// seeding RNG is not ready, it will retry later, after `threshold / 256`
    /// generated bytes. On other errors in the source RNG, this will skip
    /// reseeding and continue using the internal PRNG, until another
    /// `threshold` bytes have been generated (at which point it will try
    /// reseeding again).
    #[inline(never)]
    pub fn reseed(&mut self) {
        trace!("Reseeding RNG after generating {} bytes",
               self.threshold - self.bytes_until_reseed);
        self.bytes_until_reseed = self.threshold;
        let mut err_count = 0;
        loop {
            if let Err(e) = R::from_rng(&mut self.reseeder)
                            .map(|result| self.rng = result) {
                let kind = e.kind();
                if kind.should_wait() {
                    self.bytes_until_reseed = self.threshold >> 8;
                    warn!("Reseeding RNG delayed for {} bytes",
                           self.bytes_until_reseed);
                } else if kind.should_retry() {
                    err_count += 1;
                    // Retry immediately for 5 times (arbitrary limit)
                    if err_count <= 5 { continue; }
                }
                warn!("Reseeding RNG failed; continuing without reseeding. Error: {}", e);
            }
            break; // Successfully reseeded, delayed, or given up.
        }
    }

    /// Reseed the internal RNG if the number of bytes that have been
    /// generated exceed the threshold.
    ///
    /// If reseeding fails, return an error with the original cause. Note that
    /// if the cause has a permanent failure, we report a transient error and
    /// skip reseeding; this means that only two error kinds can be reported
    /// from this method: `ErrorKind::Transient` and `ErrorKind::NotReady`.
    #[inline(never)]
    pub fn try_reseed(&mut self) -> Result<(), Error> {
        trace!("Reseeding RNG after {} generated bytes",
               self.threshold - self.bytes_until_reseed);
        if let Err(err) = R::from_rng(&mut self.reseeder)
                          .map(|result| self.rng = result) {
            let newkind = match err.kind() {
                a @ ErrorKind::NotReady => a,
                b @ ErrorKind::Transient => b,
                _ => {
                    self.bytes_until_reseed = self.threshold; // skip reseeding
                    ErrorKind::Transient
                }
            };
            return Err(Error::with_cause(newkind, "reseeding failed", err));
        }
        self.bytes_until_reseed = self.threshold;
        Ok(())
    }
}

impl<R: RngCore + SeedableRng, Rsdr: RngCore> RngCore for ReseedingRng<R, Rsdr> {
    fn next_u32(&mut self) -> u32 {
        let value = self.rng.next_u32();
        self.bytes_until_reseed -= 4;
        if self.bytes_until_reseed <= 0 {
            self.reseed();
        }
        value
    }

    fn next_u64(&mut self) -> u64 {
        let value = self.rng.next_u64();
        self.bytes_until_reseed -= 8;
        if self.bytes_until_reseed <= 0 {
            self.reseed();
        }
        value
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.rng.fill_bytes(dest);
        self.bytes_until_reseed -= dest.len() as i64;
        if self.bytes_until_reseed <= 0 {
            self.reseed();
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.rng.try_fill_bytes(dest)?;
        self.bytes_until_reseed -= dest.len() as i64;
        if self.bytes_until_reseed <= 0 {
            self.try_reseed()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use {Rng, SeedableRng, StdRng};
    use mock::StepRng;
    use super::ReseedingRng;

    #[test]
    fn test_reseeding() {
        let mut zero = StepRng::new(0, 0);
        let rng = StdRng::from_rng(&mut zero).unwrap();
        let mut reseeding = ReseedingRng::new(rng, 32, zero);

        // Currently we only support for arrays up to length 32.
        // TODO: cannot generate seq via Rng::gen because it uses different alg
        let mut buf = [0u8; 32];
        reseeding.fill(&mut buf);
        let seq = buf;
        for _ in 0..10 {
            reseeding.fill(&mut buf);
            assert_eq!(buf, seq);
        }
    }
}
