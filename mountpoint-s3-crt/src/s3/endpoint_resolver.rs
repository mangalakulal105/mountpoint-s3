//! Endpoint resolver helps determine which S3 endpoint to use for a given configuration.

use mountpoint_s3_crt_sys::*;
use std::{
    ffi::{OsStr, OsString},
    os::unix::prelude::OsStrExt,
    ptr::{self, NonNull},
};

use crate::{
    aws_byte_cursor_as_slice,
    common::{allocator::Allocator, error::Error},
    CrtError, ToAwsByteCursor,
};

use super::s3_library_init;

/// [RuleEngine] to resolve endpoint with given [RequestContext] and ruleset
#[derive(Debug)]
pub struct RuleEngine {
    inner: NonNull<aws_endpoints_rule_engine>,
}

impl RuleEngine {
    /// Creates a new endpoint [RuleEngine].
    pub fn new(allocator: &Allocator) -> Result<Self, Error> {
        s3_library_init(allocator);
        // SAFETY: `allocator.inner` is a valid aws_allocator and and we check the return is non-null.
        // SAFETY: aws_s3_endpoint_resolver_new will acquire a reference to keep it alive after this function call, so it's safe to return an owned version to it.
        let inner = unsafe { aws_s3_endpoint_resolver_new(allocator.inner.as_ptr()).ok_or_last_error()? };
        Ok(Self { inner })
    }

    /// Resolve the endpoint with given [RequestContext] and ruleset.
    pub fn resolve(&self, context: RequestContext) -> Result<ResolvedEndpoint, Error> {
        // SAFETY: `aws_endpoints_rule_engine_resolve` ensures that it returns a `aws_endpoints_resolved_endpoint`
        // (which is checked for being non-null later) or it will return an error. So, the out_endpoint is valid and non null.
        // SAFETY: Next, we handle all the arms `aws_endpoints_resolved_endpoint_get_type` separately.
        unsafe {
            let mut out_endpoint: *mut aws_endpoints_resolved_endpoint = ptr::null_mut();
            aws_endpoints_rule_engine_resolve(self.inner.as_ptr(), context.inner.as_ptr(), &mut out_endpoint)
                .ok_or_last_error()?;
            match aws_endpoints_resolved_endpoint_get_type(out_endpoint) {
                aws_endpoints_resolved_endpoint_type::AWS_ENDPOINTS_RESOLVED_ERROR => {
                    let mut out_error: aws_byte_cursor = Default::default();
                    let err = Error::from(aws_endpoints_resolved_endpoint_get_error(out_endpoint, &mut out_error));
                    Err(err)
                }
                aws_endpoints_resolved_endpoint_type::AWS_ENDPOINTS_RESOLVED_ENDPOINT => {
                    let out_endpoint = out_endpoint.ok_or_last_error()?;
                    Ok(ResolvedEndpoint { inner: out_endpoint })
                }
                _ => unreachable!("Invalid resolved endpoint type"),
            }
        }
    }
}

impl Drop for RuleEngine {
    fn drop(&mut self) {
        // SAFETY: `self.inner` is a valid `aws_endpoints_rule_engine`, and on Drop it's safe to decrement
        // the reference count since we won't use it again through `self`.
        unsafe {
            aws_endpoints_rule_engine_release(self.inner.as_ptr());
        }
    }
}

/// Stores context about the endpoint configuration for use by the endpoint resolver
#[derive(Debug)]
pub struct RequestContext {
    inner: NonNull<aws_endpoints_request_context>,
}

impl RequestContext {
    /// Creates a new endpoint [RequestContext].
    pub fn new(allocator: &Allocator) -> Result<Self, Error> {
        s3_library_init(allocator);
        // SAFETY: `allocator.inner` is a valid aws_allocator and and we check the return is non-null.
        // SAFETY: aws_endpoints_request_context_new will acquire a reference to keep it alive after this function call, so it's safe to return an owned version to it.
        let inner = unsafe { aws_endpoints_request_context_new(allocator.inner.as_ptr()).ok_or_last_error()? };
        Ok(Self { inner })
    }

    /// Add the parameter to [RequestContext] whose value is in form of string
    pub fn add_string(&mut self, allocator: &Allocator, name: &OsStr, value: &OsStr) -> Result<(), Error> {
        // SAFETY: allocator.inner and self.inner should be valid pointers.
        // `name` and `value` should be valid aws byte cursor not be modified further.
        unsafe {
            aws_endpoints_request_context_add_string(
                allocator.inner.as_ptr(),
                self.inner.as_ptr(),
                name.as_aws_byte_cursor(),
                value.as_aws_byte_cursor(),
            )
            .ok_or_last_error()
        }
    }

    /// Add the parameter to [RequestContext] whose value is in form of boolean
    pub fn add_boolean(&mut self, allocator: &Allocator, name: &OsStr, value: bool) -> Result<(), Error> {
        // SAFETY: allocator.inner and self.inner should be valid pointers.
        // `name` will be copied by CRT, thus borrowing is safe
        unsafe {
            aws_endpoints_request_context_add_boolean(
                allocator.inner.as_ptr(),
                self.inner.as_ptr(),
                name.as_aws_byte_cursor(),
                value,
            )
            .ok_or_last_error()
        }
    }
}

impl Drop for RequestContext {
    fn drop(&mut self) {
        // SAFETY: `self.inner` is a valid `aws_endpoints_request_context`, and on Drop it's safe to decrement
        // the reference count since we won't use it again through `self.`
        unsafe {
            aws_endpoints_request_context_release(self.inner.as_ptr());
        }
    }
}

/// [ResolvedEndpoint] for the given context using [RuleEngine] and rule set
#[derive(Debug)]
pub struct ResolvedEndpoint {
    inner: NonNull<aws_endpoints_resolved_endpoint>,
}

impl ResolvedEndpoint {
    /// Get URI from the [ResolvedEndpoint]
    pub fn get_url(&self) -> Result<OsString, Error> {
        let mut url: aws_byte_cursor = Default::default();
        // SAFETY: self.inner is valid pointer to resolved endpoint as it is supposed to be used after Resolve(). url is passed as valid mutable pointer.
        //`aws_endpoint_resolved_enpoint_get_url` ensures to return an initialized aws_byte_cursor for url or else it will return an error.
        unsafe {
            aws_endpoints_resolved_endpoint_get_url(self.inner.as_ptr(), &mut url).ok_or_last_error()?;
        }
        // SAFETY: `uri` does not outlive the aws_byte_cursor `url` as an owned OsString is returned rather than reference to a slice.
        let uri = unsafe { aws_byte_cursor_as_slice(&url) };
        Ok(OsStr::from_bytes(uri).to_os_string())
    }
}

impl Drop for ResolvedEndpoint {
    fn drop(&mut self) {
        // SAFETY: `self.inner` is a valid `aws_endpoints_resolved_endpoint`, and on Drop it's safe to decrement
        // the reference count since we won't use it again through `self.`
        unsafe {
            aws_endpoints_resolved_endpoint_release(self.inner.as_ptr());
        }
    }
}

#[cfg(test)]
mod test {
    use std::ffi::OsStr;

    use crate::common::allocator::Allocator;

    use super::{RequestContext, RuleEngine};

    #[test]
    fn test_regions_outside_aws_partition() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Bucket"), OsStr::new("s3-bucket-test"))
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("cn-north-1"))
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(endpoint_uri, "https://s3-bucket-test.s3.cn-north-1.amazonaws.com.cn");
    }

    #[test]
    fn test_default_behaviour_aws_partition() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Bucket"), OsStr::new("s3-bucket-test"))
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("eu-west-1"))
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(endpoint_uri, "https://s3-bucket-test.s3.eu-west-1.amazonaws.com");
        // Adding a '.' in the bucket name
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Bucket"), OsStr::new("s3-bucket.test"))
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("eu-west-1"))
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(endpoint_uri, "https://s3.eu-west-1.amazonaws.com/s3-bucket.test");
    }

    #[test]
    fn test_fips_setting() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Bucket"), OsStr::new("s3-bucket-test"))
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("us-east-1"))
            .unwrap();
        endpoint_request_context
            .add_boolean(&new_allocator, OsStr::new("UseFIPS"), true)
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(endpoint_uri, "https://s3-bucket-test.s3-fips.us-east-1.amazonaws.com");
    }

    #[test]
    fn test_transfer_acceleration_dual_stack_setting() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Bucket"), OsStr::new("s3-bucket-test"))
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("us-east-1"))
            .unwrap();
        endpoint_request_context
            .add_boolean(&new_allocator, OsStr::new("UseDualStack"), true)
            .unwrap();
        endpoint_request_context
            .add_boolean(&new_allocator, OsStr::new("Accelerate"), true)
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(
            endpoint_uri,
            "https://s3-bucket-test.s3-accelerate.dualstack.amazonaws.com"
        );
    }

    #[test]
    fn test_force_path_style_setting() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Bucket"), OsStr::new("s3-bucket-test"))
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("eu-west-1"))
            .unwrap();
        endpoint_request_context
            .add_boolean(&new_allocator, OsStr::new("ForcePathStyle"), true)
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(endpoint_uri, "https://s3.eu-west-1.amazonaws.com/s3-bucket-test");
    }

    #[test]
    fn test_access_point_alias() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(
                &new_allocator,
                OsStr::new("Bucket"),
                OsStr::new("mountpoint-o-o0f129d3877cfede5ed9w5rxpf30v7bhaa83yause10--op-s3"),
            )
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("us-east-1"))
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(endpoint_uri, "https://mountpoint-o-o0f129d3877cfede5ed9w5rxpf30v7bhaa83yause10--op-s3.op-0f129d3877cfede5e.s3-outposts.us-east-1.amazonaws.com");
    }

    #[test]
    fn test_arn_as_bucket_name() {
        let new_allocator = Allocator::default();
        let endpoint_rule_engine = RuleEngine::new(&new_allocator).unwrap();
        let mut endpoint_request_context = RequestContext::new(&new_allocator).unwrap();
        endpoint_request_context
            .add_string(
                &new_allocator,
                OsStr::new("Bucket"),
                OsStr::new("arn:aws:s3::accountID:accesspoint/mfzwi23gnjvgw.mrap"),
            )
            .unwrap();
        endpoint_request_context
            .add_string(&new_allocator, OsStr::new("Region"), OsStr::new("eu-west-1"))
            .unwrap();
        let endpoint_resolved = endpoint_rule_engine
            .resolve(endpoint_request_context)
            .expect("endpoint should resolve as rules should match context");
        let endpoint_uri = endpoint_resolved.get_url().unwrap();
        assert_eq!(
            endpoint_uri.as_os_str(),
            "https://mfzwi23gnjvgw.mrap.accesspoint.s3-global.amazonaws.com"
        );
    }
}
