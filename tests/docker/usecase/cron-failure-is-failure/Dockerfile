FROM sysg-usecase-base

COPY tests/docker/usecase/cron-failure-is-failure/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/cron-failure-is-failure/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
