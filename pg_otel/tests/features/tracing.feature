Feature: Tracing and Spans

  Scenario: Simple Utility
    One utility statement that does nothing with rows/data should produce one span.
    This is "autocommit" and the implicit transaction does *not* produce a span.

    Given I am authenticated
    When I execute: CREATE TEMP TABLE t1 (id int)
    Then I should see a span with:
      | kind      | server         |
      | status    | ok             |
      | span_id   | *generated*    |
      | trace_id  | *generated*    |
      | parent_id | *none*         |
      | db.operation.name | CREATE |

  Scenario: Utility with Context
    Context attached to one utility statement appears on the produced span.

    Given I am authenticated
    When I execute: CREATE TEMP TABLE t1 (id int) /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */
    Then I should see a span with:
      | kind      | server           |
      | status    | ok               |
      | span_id   | *generated*      |
      | trace_id  | 11111111111111111111111111111111 |
      | parent_id | 2222222222222222 |
      | db.operation.name | CREATE   |

  Scenario: Simple Query
    One query without parallelization should produce one span.
    This is "autocommit" and the implicit transaction does *not* produce a span.

    Given I am authenticated
    When I execute: SELECT 1
    Then I should see a span with:
      | kind      | server         |
      | status    | ok             |
      | span_id   | *generated*    |
      | trace_id  | *generated*    |
      | parent_id | *none*         |
      | db.operation.name | SELECT |

  Scenario: Simple Query Error
    A query that raises an error should produce a span with an error status and SQL state.

    Given I am authenticated
    When I execute: SELECT 1/0
    Then I should see a span with:
      | kind      | server      |
      | status    | error       |
      | span_id   | *generated* |
      | trace_id  | *generated* |
      | parent_id | *none*      |
      | db.operation.name       | SELECT |
      | db.response.status_code | 22012  |

  Scenario: Query with Comment Context
    Context attached to one query appears on the produced span.

    Given I am authenticated
    When I execute: SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */
    Then I should see a span with:
      | kind      | server           |
      | status    | ok               |
      | span_id   | *generated*      |
      | trace_id  | 11111111111111111111111111111111 |
      | parent_id | 2222222222222222 |
      | db.operation.name | SELECT   |

  Scenario: Multiple Query
    Two queries without parallelization should produce two spans.
    The Span IDs should differ and the Trace IDs should differ.
    This is "autocommit" and the implicit transaction does *not* produce a span.

    Given I am authenticated
    When I execute: SELECT 1
    When I execute: SELECT 2
    Then I should see a span with:
      | kind      | server         |
      | status    | ok             |
      | span_id   | *generated*    |
      | trace_id  | *generated*    |
      | parent_id | *none*         |
      | db.operation.name | SELECT |
    And I should see a span with:
      | kind      | server         |
      | status    | ok             |
      | span_id   | *generated*    |
      | trace_id  | *generated*    |
      | parent_id | *none*         |
      | db.operation.name | SELECT |

  Scenario: Query with GUC Context
    Context attached to one query appears on one produced span.
    The second query here has different Span ID and Trace ID and no parent.

    Given I am authenticated
    When I execute: SET pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'
    When I execute: SELECT 1
    When I execute: SELECT 2
    Then I should see a span with:
      | kind      | server           |
      | status    | ok               |
      | span_id   | *generated*      |
      | trace_id  | 11111111111111111111111111111111 |
      | parent_id | 2222222222222222 |
      | db.operation.name | SELECT   |
    And I should see a span with:
      | kind      | server           |
      | status    | ok               |
      | span_id   | *generated*      |
      | trace_id  | *generated*      |
      | parent_id | *none*           |
      | db.operation.name | SELECT   |

  Scenario: Single Query in Transaction
    An explicit transaction statement produces a transaction span with child query and commit spans.

    Given I am authenticated
    When I execute: BEGIN
    When I execute: SELECT 1
    When I execute: COMMIT
    Then I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *generated*   |
      | parent_id         | *none*        |
      | db.operation.name | TRANSACTION   |
    And I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *transaction* |
      | parent_id         | *transaction* |
      | db.operation.name | SELECT        |
    And I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *transaction* |
      | parent_id         | *transaction* |
      | db.operation.name | COMMIT        |

  Scenario: Multiple Queries in Transaction
    An explicit transaction statement is the parent of every span up to and including the commit span.

    Given I am authenticated
    When I execute: BEGIN
    When I execute: SELECT 1
    When I execute: SELECT 2
    When I execute: COMMIT
    Then I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *generated*   |
      | parent_id         | *none*        |
      | db.operation.name | TRANSACTION   |
    And I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *transaction* |
      | parent_id         | *transaction* |
      | db.operation.name | SELECT        |
    And I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *transaction* |
      | parent_id         | *transaction* |
      | db.operation.name | SELECT        |
    And I should see a span with:
      | kind              | server        |
      | status            | ok            |
      | span_id           | *generated*   |
      | trace_id          | *transaction* |
      | parent_id         | *transaction* |
      | db.operation.name | COMMIT        |

  Scenario: Single Query in Transaction with Context
    Context attached to an explicit transaction appears on the transaction span.

    Given I am authenticated
    When I execute: BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */
    When I execute: SELECT 1
    When I execute: COMMIT
    Then I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | 2222222222222222 |
      | db.operation.name | TRANSACTION      |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | COMMIT           |

  Scenario: Single Query with Context in Transaction
    Without SET or SET LOCAL, the transaction has its own trace.
    Context attached to a single statement causes a Link to the transaction.

    Given I am authenticated
    When I execute: BEGIN
    When I execute: SELECT 1 /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */
    When I execute: COMMIT
    Then I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | *generated*      |
      | parent_id         | *none*           |
      | db.operation.name | TRANSACTION      |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | 2222222222222222 |
      | link_span_id      | *transaction*    |
      | link_trace_id     | *transaction*    |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | *transaction*    |
      | parent_id         | *transaction*    |
      | db.operation.name | COMMIT           |

  Scenario: Parent Override in Transaction with Context
    Context attached to a statement appears only on that statement's span.
    The Trace of its former/otherwise parent is associated as a Link.

    Given I am authenticated
    When I execute: BEGIN    /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */
    When I execute: SELECT 1 /* traceparent='00-33333333333333333333333333333333-4444444444444444-01' */
    When I execute: SELECT 2
    When I execute: COMMIT
    Then I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | 2222222222222222 |
      | db.operation.name | TRANSACTION      |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 33333333333333333333333333333333 |
      | parent_id         | 4444444444444444 |
      | link_span_id      | *transaction*    |
      | link_trace_id     | 11111111111111111111111111111111 |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | COMMIT           |

  Scenario: Query Error in Transaction
    An aborted transaction should have an error status on its span.
    The span that caused the error should have an error status and the SQLSTATE.
    The span for the transaction rollback should not have an error when it succeeds.

    Given I am authenticated
    When I execute: BEGIN
    When I execute: SELECT 1/0
    When I execute: ROLLBACK
    Then I should see a span with:
      | kind                    | server        |
      | status                  | error         |
      | span_id                 | *generated*   |
      | trace_id                | *generated*   |
      | parent_id               | *none*        |
      | db.operation.name       | TRANSACTION   |
    And I should see a span with:
      | kind                    | server        |
      | status                  | error         |
      | span_id                 | *generated*   |
      | trace_id                | *transaction* |
      | parent_id               | *transaction* |
      | db.operation.name       | SELECT        |
      | db.response.status_code | 22012         |
    And I should see a span with:
      | kind                    | server        |
      | status                  | ok            |
      | span_id                 | *generated*   |
      | trace_id                | *transaction* |
      | parent_id               | *transaction* |
      | db.operation.name       | ROLLBACK      |

  Scenario: Transaction Commit Failure
    A transaction that aborts at commit should have an error status and SQLSTATE.
    Query spans that succeeded before commit should not have errors.
    The span for the transaction commit should have an error status and SQLSTATE.

    Given I am authenticated
    When I execute: CREATE TEMP TABLE p (id int PRIMARY KEY)
    When I execute: CREATE TEMP TABLE c (p_id int REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED)
    When I execute: BEGIN
    When I execute: INSERT INTO c VALUES (999)
    When I execute: COMMIT
    Then I should see a span with:
      | kind                    | server        |
      | status                  | error         |
      | span_id                 | *generated*   |
      | trace_id                | *generated*   |
      | parent_id               | *none*        |
      | db.operation.name       | TRANSACTION   |
      | db.response.status_code | 23503         |
    And I should see a span with:
      | kind                    | server        |
      | status                  | ok            |
      | span_id                 | *generated*   |
      | trace_id                | *transaction* |
      | parent_id               | *transaction* |
      | db.operation.name       | INSERT        |
    And I should see a span with:
      | kind                    | server        |
      | status                  | error         |
      | span_id                 | *generated*   |
      | trace_id                | *transaction* |
      | parent_id               | *transaction* |
      | db.operation.name       | COMMIT        |
      | db.response.status_code | 23503         |

  Scenario: Transaction with Pooler GUC Context
    Context applies only to immediately following statement or transaction.

    Given I am authenticated
    When I execute: SET pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'
    When I execute: BEGIN
    When I execute: SELECT 1
    When I execute: COMMIT
    When I execute: SELECT 2
    Then I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | 2222222222222222 |
      | db.operation.name | TRANSACTION      |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | COMMIT           |
    And I should see a span with:
      | kind      | server         |
      | status    | ok             |
      | span_id   | *generated*    |
      | trace_id  | *generated*    |
      | parent_id | *none*         |
      | db.operation.name | SELECT |

  Scenario: Transaction with SET LOCAL Context
    Context can be applied to a transaction when its first statement is a SET LOCAL.

    Given I am authenticated
    When I execute: BEGIN
    When I execute: SET LOCAL pg_otel.traceparent = '00-11111111111111111111111111111111-2222222222222222-01'
    When I execute: SELECT 1
    When I execute: COMMIT
    When I execute: SELECT 2
    Then I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | 2222222222222222 |
      | db.operation.name | TRANSACTION      |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | COMMIT           |
    And I should see a span with:
      | kind      | server         |
      | status    | ok             |
      | span_id   | *generated*    |
      | trace_id  | *generated*    |
      | parent_id | *none*         |
      | db.operation.name | SELECT |

  Scenario: Parent Override in Transaction with SET LOCAL Context
    Context cannot be applied to a transaction twice.
    The second statement here applies to the statement that follows it.

    Given I am authenticated
    When I execute: BEGIN /* traceparent='00-11111111111111111111111111111111-2222222222222222-01' */
    When I execute: SET LOCAL pg_otel.traceparent = '00-33333333333333333333333333333333-4444444444444444-01'
    When I execute: SELECT 1
    When I execute: SELECT 2
    When I execute: COMMIT
    Then I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | 2222222222222222 |
      | db.operation.name | TRANSACTION      |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 33333333333333333333333333333333 |
      | parent_id         | 4444444444444444 |
      | link_span_id      | *transaction*    |
      | link_trace_id     | 11111111111111111111111111111111 |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | SELECT           |
    And I should see a span with:
      | kind              | server           |
      | status            | ok               |
      | span_id           | *generated*      |
      | trace_id          | 11111111111111111111111111111111 |
      | parent_id         | *transaction*    |
      | db.operation.name | COMMIT           |

