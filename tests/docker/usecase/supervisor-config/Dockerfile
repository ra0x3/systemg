FROM sysg-usecase-base

COPY tests/docker/usecase/supervisor-config/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/supervisor-config/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
