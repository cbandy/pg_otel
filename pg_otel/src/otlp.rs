// SPDX-License-Identifier: MIT

pub use self::ext::*;
use std::ffi;

pub use opentelemetry_proto::tonic::{
    common::v1::{AnyValue, ArrayValue, InstrumentationScope, KeyValue, KeyValueList},
    logs::v1::{LogRecord, LogsData, ResourceLogs, ScopeLogs, SeverityNumber as LogSeverity},
    resource::v1::Resource,
};

pub fn new_str_unchecked(v: *const ffi::c_char) -> AnyValue {
    AnyValue::new_string(unsafe {
        String::from_utf8_unchecked(ffi::CStr::from_ptr(v).to_bytes().into())
    })
}

pub struct KeyValueListBuilder(Vec<KeyValue>);

impl KeyValueListBuilder {
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    pub fn finish(self) -> KeyValueList {
        KeyValueList::new(self.0)
    }

    pub fn int<T: Into<i64>>(&mut self, k: &str, v: T) -> &mut Self {
        self.0.push(KeyValue::new(k, AnyValue::new_int(v)));
        self
    }

    pub fn str_unchecked(&mut self, k: &str, v: *const ffi::c_char) -> &mut Self {
        self.0.push(KeyValue::new(k, new_str_unchecked(v)));
        self
    }
}

/// This module extends the upstream bindings with chainable constructors, builders, and setters.
/// Each target type implements its own `<Type>Ext` trait.
///
/// Types with few fields get:
///  - a `fn new(…) -> Self` function
///  - and `.set_<field>(mut self, value) -> Self` methods for chaining
///
/// Types with many fields use a builder pattern:
///  - a `fn build() -> <Type>Builder` function
///  - a `<Type>Builder` newtype over `<Type>`
///  - a `Default` constructor for the Builder
///  - a `fn <field>(mut self, value: impl Into<…>) -> Self` method on the Builder for every field
///  - a `.finish(self) -> <Type>` method on the Builder that returns the inner type
///
pub mod ext {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;

    impl AnyValueExt for AnyValue {}
    #[allow(dead_code)]
    pub trait AnyValueExt {
        #[allow(clippy::new_ret_no_self)]
        fn new(v: Value) -> AnyValue {
            AnyValue { value: Some(v) }
        }

        fn new_int<T: Into<i64>>(v: T) -> AnyValue {
            AnyValue::new(Value::IntValue(v.into()))
        }

        fn new_double(v: f64) -> AnyValue {
            AnyValue::new(Value::DoubleValue(v))
        }

        fn new_bool(v: bool) -> AnyValue {
            AnyValue::new(Value::BoolValue(v))
        }

        fn new_string<T: Into<String>>(v: T) -> AnyValue {
            AnyValue::new(Value::StringValue(v.into()))
        }

        fn new_bytes<T: Into<Vec<u8>>>(v: T) -> AnyValue {
            AnyValue::new(Value::BytesValue(v.into()))
        }

        fn new_kvlist<T: IntoIterator<Item = KeyValue>>(v: T) -> AnyValue {
            AnyValue::new(Value::KvlistValue(KeyValueList {
                values: v.into_iter().collect(),
            }))
        }

        fn new_array<T: IntoIterator<Item = AnyValue>>(v: T) -> AnyValue {
            AnyValue::new(Value::ArrayValue(ArrayValue {
                values: v.into_iter().collect(),
            }))
        }
    }

    impl KeyValueExt for KeyValue {}
    pub trait KeyValueExt {
        #[allow(clippy::new_ret_no_self)]
        fn new<K: Into<String>>(k: K, v: AnyValue) -> KeyValue {
            KeyValue {
                key: k.into(),
                value: Some(v),
                key_strindex: 0,
            }
        }
    }

    impl KeyValueListExt for KeyValueList {}
    pub trait KeyValueListExt {
        #[allow(clippy::new_ret_no_self)]
        fn new<T: IntoIterator<Item = KeyValue>>(values: T) -> KeyValueList {
            KeyValueList {
                values: values.into_iter().collect(),
            }
        }
    }

    impl LogRecordExt for LogRecord {}
    pub trait LogRecordExt {
        fn build() -> LogRecordBuilder {
            LogRecordBuilder::default()
        }
    }

    #[derive(Default)]
    pub struct LogRecordBuilder(LogRecord);
    #[allow(dead_code)]
    impl LogRecordBuilder {
        pub fn time_unix_nano(mut self, v: u64) -> Self {
            self.0.time_unix_nano = v;
            self
        }

        pub fn observed_time_unix_nano(mut self, v: u64) -> Self {
            self.0.observed_time_unix_nano = v;
            self
        }

        pub fn severity_number(mut self, v: impl Into<i32>) -> Self {
            self.0.severity_number = v.into();
            self
        }

        pub fn severity_text(mut self, v: impl Into<String>) -> Self {
            self.0.severity_text = v.into();
            self
        }

        pub fn body(mut self, v: AnyValue) -> Self {
            self.0.body = Some(v);
            self
        }

        pub fn attributes(mut self, v: impl IntoIterator<Item = KeyValue>) -> Self {
            self.0.attributes = v.into_iter().collect();
            self
        }

        pub fn dropped_attributes_count(mut self, v: u32) -> Self {
            self.0.dropped_attributes_count = v;
            self
        }

        pub fn flags(mut self, v: impl Into<u32>) -> Self {
            self.0.flags = v.into();
            self
        }

        pub fn trace_id(mut self, v: impl Into<Vec<u8>>) -> Self {
            self.0.trace_id = v.into();
            self
        }

        pub fn span_id(mut self, v: impl Into<Vec<u8>>) -> Self {
            self.0.span_id = v.into();
            self
        }

        pub fn event_name(mut self, v: impl Into<String>) -> Self {
            self.0.event_name = v.into();
            self
        }

        pub fn finish(self) -> LogRecord {
            self.0
        }
    }

    impl ResourceExt for Resource {}
    pub trait ResourceExt {
        fn build() -> ResourceBuilder {
            ResourceBuilder::default()
        }
    }

    #[derive(Default)]
    pub struct ResourceBuilder(Resource);
    #[allow(dead_code)]
    impl ResourceBuilder {
        pub fn attributes(mut self, v: impl IntoIterator<Item = KeyValue>) -> Self {
            self.0.attributes = v.into_iter().collect();
            self
        }

        pub fn dropped_attributes_count(mut self, v: u32) -> Self {
            self.0.dropped_attributes_count = v;
            self
        }

        pub fn finish(self) -> Resource {
            self.0
        }
    }

    impl InstrumentationScopeExt for InstrumentationScope {}
    pub trait InstrumentationScopeExt {
        fn build() -> InstrumentationScopeBuilder {
            InstrumentationScopeBuilder::default()
        }
    }

    #[derive(Default)]
    pub struct InstrumentationScopeBuilder(InstrumentationScope);
    #[allow(dead_code)]
    impl InstrumentationScopeBuilder {
        pub fn name(mut self, v: impl Into<String>) -> Self {
            self.0.name = v.into();
            self
        }

        pub fn version(mut self, v: impl Into<String>) -> Self {
            self.0.version = v.into();
            self
        }

        pub fn attributes(mut self, v: impl IntoIterator<Item = KeyValue>) -> Self {
            self.0.attributes = v.into_iter().collect();
            self
        }

        pub fn dropped_attributes_count(mut self, v: u32) -> Self {
            self.0.dropped_attributes_count = v;
            self
        }

        pub fn finish(self) -> InstrumentationScope {
            self.0
        }
    }

    impl ScopeLogsExt for ScopeLogs {}
    pub trait ScopeLogsExt {
        #[allow(clippy::new_ret_no_self)]
        fn new<S, T>(scope: S, log_records: T) -> ScopeLogs
        where
            S: Into<Option<InstrumentationScope>>,
            T: IntoIterator<Item = LogRecord>,
        {
            ScopeLogs {
                scope: scope.into(),
                log_records: log_records.into_iter().collect(),
                schema_url: String::new(),
            }
        }
    }

    impl ResourceLogsExt for ResourceLogs {}
    pub trait ResourceLogsExt {
        #[allow(clippy::new_ret_no_self)]
        fn new<R, T>(resource: R, scope_logs: T) -> ResourceLogs
        where
            R: Into<Option<Resource>>,
            T: IntoIterator<Item = ScopeLogs>,
        {
            ResourceLogs {
                resource: resource.into(),
                scope_logs: scope_logs.into_iter().collect(),
                schema_url: String::new(),
            }
        }
    }

    impl LogsDataExt for LogsData {}
    pub trait LogsDataExt {
        #[allow(clippy::new_ret_no_self)]
        fn new<T: IntoIterator<Item = ResourceLogs>>(resource_logs: T) -> LogsData {
            LogsData {
                resource_logs: resource_logs.into_iter().collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;

    #[test]
    fn test_any_value() {
        let int_val = AnyValue::new_int(42i64);
        assert_eq!(int_val.value, Some(Value::IntValue(42)));

        let double_val = AnyValue::new_double(3.15);
        assert_eq!(double_val.value, Some(Value::DoubleValue(3.15)));

        let bool_val = AnyValue::new_bool(true);
        assert_eq!(bool_val.value, Some(Value::BoolValue(true)));

        let string_val = AnyValue::new_string("hello");
        assert_eq!(
            string_val.value,
            Some(Value::StringValue("hello".to_string()))
        );

        let bytes_val = AnyValue::new_bytes(vec![1, 2, 3]);
        assert_eq!(bytes_val.value, Some(Value::BytesValue(vec![1, 2, 3])));
    }

    #[test]
    fn test_key_value() {
        let kv = KeyValue::new("key", AnyValue::new_string("value"));
        assert_eq!(kv.key, "key");
        assert_eq!(
            kv.value.unwrap().value,
            Some(Value::StringValue("value".to_string()))
        );
    }

    #[test]
    fn test_log_record_builder() {
        let record = LogRecord::build()
            .time_unix_nano(100)
            .observed_time_unix_nano(101)
            .severity_number(LogSeverity::Info)
            .severity_text("INFO")
            .body(AnyValue::new_string("test log message"))
            .finish();

        assert_eq!(record.time_unix_nano, 100);
        assert_eq!(record.observed_time_unix_nano, 101);
        assert_eq!(record.severity_number, LogSeverity::Info as i32);
        assert_eq!(record.severity_text, "INFO");
        assert_eq!(
            record.body.unwrap().value,
            Some(Value::StringValue("test log message".to_string()))
        );
    }

    #[test]
    fn test_instrumentation_scope_builder() {
        let scope = InstrumentationScope::build()
            .name("my-scope")
            .version("1.0.0")
            .finish();

        assert_eq!(scope.name, "my-scope");
        assert_eq!(scope.version, "1.0.0");
    }

    #[test]
    fn test_logs_hierarchy() {
        let record = LogRecord::build()
            .time_unix_nano(200)
            .body(AnyValue::new_string("log message"))
            .finish();

        let scope = InstrumentationScope::build().name("scope").finish();

        let resource = Resource::build().finish();

        let mut scope_logs = ScopeLogs::new(scope, vec![record]);
        scope_logs.schema_url = "http://schema.url".into();
        let resource_logs = ResourceLogs::new(resource, vec![scope_logs]);
        let logs_data = LogsData::new(vec![resource_logs]);

        assert_eq!(logs_data.resource_logs.len(), 1);
        assert_eq!(logs_data.resource_logs[0].scope_logs.len(), 1);
        assert_eq!(
            logs_data.resource_logs[0].scope_logs[0].log_records.len(),
            1
        );
        assert_eq!(
            logs_data.resource_logs[0].scope_logs[0].schema_url,
            "http://schema.url"
        );
    }
}
