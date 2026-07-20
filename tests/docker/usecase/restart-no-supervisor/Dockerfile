FROM sysg-usecase-base

COPY tests/docker/usecase/restart-no-supervisor/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/restart-no-supervisor/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
