package stdin

import (
	"testing"

	"go.opentelemetry.io/collector/model/pdata"
	"gotest.tools/v3/assert"
)

func TestParseLine(t *testing.T) {
	_, err := parseLine([]byte(`nope`))
	assert.ErrorContains(t, err, "expected integer")

	_, err = parseLine([]byte(`1,`))
	assert.ErrorContains(t, err, "too short")

	_, err = parseLine([]byte(`2,[]`))
	assert.ErrorContains(t, err, "unsupported version")
}

func TestParseV1(t *testing.T) {
	_, err := parseV1([]byte(`""`))
	assert.ErrorContains(t, err, "got string")

	_, err = parseV1([]byte(`[]`))
	assert.ErrorContains(t, err, "invalid")

	_, err = parseV1([]byte(`[
		99, "", "", "extra"
	]`))
	assert.ErrorContains(t, err, "too many")

	record, err := parseV1([]byte(`[
		12345, "INFO", "some message"
	]`))
	assert.NilError(t, err)

	assert.Equal(t, record.Timestamp(), pdata.Timestamp(12345))
	assert.Equal(t, record.SeverityText(), "INFO")
	assert.Equal(t, record.Body().StringVal(), "some message",
		"type %v", record.Body().Type())
}
