package stdin

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"

	"go.opentelemetry.io/collector/component"
	"go.opentelemetry.io/collector/consumer"
	"go.opentelemetry.io/collector/model/pdata"
	"go.uber.org/zap"
)

type receiver struct {
	component.ReceiverCreateSettings

	source io.ReadCloser
	sink   consumer.Logs

	finished chan struct{}
}

func (r *receiver) Start(ctx context.Context, host component.Host) error {
	// Start tells the component to start. Host parameter can be used for communicating
	// with the host after Start() has already returned. If an error is returned by
	// Start() then the collector startup will be aborted.
	// If this is an exporter component it may prepare for exporting
	// by connecting to the endpoint.
	//
	// If the component needs to perform a long-running starting operation then it is recommended
	// that Start() returns quickly and the long-running operation is performed in background.
	// In that case make sure that the long-running operation does not use the context passed
	// to Start() function since that context will be cancelled soon and can abort the long-running
	// operation. Create a new context from the context.Background() for long-running operations.

	scanner := bufio.NewScanner(r.source)
	started := make(chan struct{})
	defer close(started)

	r.finished = make(chan struct{})
	go func() {
		defer close(r.finished)
		<-started

		for scanner.Scan() {
			b := scanner.Bytes()
			if len(b) == 0 {
				continue
			}

			record, err := parseLine(b)
			if err != nil {
				r.Logger.Warn("Invalid line", zap.Error(err))
				continue
			}

			message := pdata.NewLogs()
			record.CopyTo(message.
				ResourceLogs().AppendEmpty().
				InstrumentationLibraryLogs().AppendEmpty().
				Logs().AppendEmpty())

			if err := r.sink.ConsumeLogs(context.TODO(), message); err != nil {
				r.Logger.Error("Next consumer failed", zap.Error(err))
			}
		}

		err := scanner.Err()
		if err != nil && !errors.Is(err, io.EOF) && !errors.Is(err, os.ErrClosed) {
			host.ReportFatalError(err)
		}
	}()

	return nil
}

func (r *receiver) Shutdown(ctx context.Context) error {
	// Shutdown is invoked during service shutdown. After Shutdown() is called, if the component
	// accepted data in any way, it should not accept it anymore.
	//
	// If there are any background operations running by the component they must be aborted before
	// this function returns. Remember that if you started any long-running background operations from
	// the Start() method, those operations must be also cancelled. If there are any buffers in the
	// component, they should be cleared and the data sent immediately to the next component.
	//
	// The component's lifecycle is completed once the Shutdown() method returns. No other
	// methods of the component are called after that. If necessary a new component with
	// the same or different configuration may be created and started (this may happen
	// for example if we want to restart the component).

	err := r.source.Close()
	if errors.Is(err, os.ErrClosed) {
		err = nil
	}

	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-r.finished:
		return err
	}
}

func parseLine(line []byte) (pdata.LogRecord, error) {
	if len(line) < 3 {
		return pdata.LogRecord{}, fmt.Errorf("line too short: %q", line)
	}

	if line[0] == '1' && line[1] == ',' {
		return parseV1(line[2:])
	}

	var version int
	_, err := fmt.Sscanf(string(line), "%d,", &version)
	if err == nil {
		err = fmt.Errorf("unsupported version: %d", version)
	}

	return pdata.LogRecord{}, err
}

func parseV1(array []byte) (pdata.LogRecord, error) {
	decoder := json.NewDecoder(bytes.NewReader(array))
	record := pdata.NewLogRecord()

	var message, severity string
	var timestamp pdata.Timestamp

	token, err := decoder.Token()
	if delim, ok := token.(json.Delim); err == nil && (!ok || delim != '[') {
		err = fmt.Errorf("expected JSON array, got %T", token)
	}
	if err == nil {
		err = decoder.Decode(&timestamp)
	}
	if err == nil {
		record.SetTimestamp(timestamp)
		err = decoder.Decode(&severity)
	}
	if err == nil {
		record.SetSeverityText(severity)
		err = decoder.Decode(&message)
	}
	if err == nil {
		record.Body().SetStringVal(message)
		token, err = decoder.Token()
	}

	// TODO more values

	if delim, ok := token.(json.Delim); err == nil && (!ok || delim != ']') {
		err = fmt.Errorf("too many values, got %T", token)
	}

	return record, err
}